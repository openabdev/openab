# Microsoft Teams Live-Validation Records

- **Status:** Working evidence ledger
- **Evidence through:** 2026-08-20
- **Current state:** [Microsoft Teams live-validation tracker](msteams-live-validation.md)

This file preserves sanitized tenant-specific test procedures, observations, and
open cases migrated from proposed Teams ADRs. It is not an ADR, product
specification, release guarantee, or current-status authority. Read the tracker
for the current matrix and the linked ADR for each durable decision.

Do not add credentials, service URLs, tenant or conversation identifiers,
message content, raw production logs, absolute event timestamps, or screenshot
paths. A later correction must preserve the prior verdict and explain the
superseding evidence rather than silently rewriting history.

## Detailed validation records

These sanitized records were migrated from proposed ADRs so those documents can
remain durable architectural decisions. This section preserves historical
observations and open tenant-specific gates; the matrix above is the current
status authority. Evidence is not a product guarantee outside the recorded
scope.

### Ingress route and duplicate publication

- **Decision:** [Teams Ephemeral Ingress State](adr/teams-ephemeral-ingress-state.md)

Verified on 2026-08-07 in a commercial-cloud Personal chat:

- with Core stopped and no WebSocket consumer, one authenticated message
  reached the no-consumer path and produced one retryable HTTP 503 warning;
- after the first 503, a single probe connected and Bot Framework retried the
  same user activity without a second user message;
- the retried activity was accepted and published to the probe exactly once;
- a route-scoped `✅` reaction returned `Delivered`, proving that the successful
  retry committed usable ingress state, and cleanup removed the reaction;
- the probe remained the only consumer and normal Core connectivity was
  restored afterward.

This confirms failed-publication rollback plus a real Microsoft retry in the
tested Personal scope. It does not independently prove a post-accept duplicate,
concurrent duplicate arbitration, crash replay, or durable delivery; those
claims remain limited to automated state-machine coverage or explicit
exclusions above.

### Normal send and explicit reply correlation

- **Decision:** [Teams Real Send Acknowledgement](adr/teams-real-send-acknowledgement.md)

The following remain explicitly **unverified** until tested in Microsoft 365:

- channel root normal-response placement;
- channel reply-to-root and reply-to-reply placement;
- whether group-chat normal responses display any quote UI;
- explicit quote presentation in group and channel scopes;
- malformed/empty activity IDs returned by the live Connector;
- long-running turns near the configured route TTL;
- live `429 Retry-After` behavior.

Complete real-send and reply-correlation support must not be claimed until the
corresponding open cases in the [current tracker](msteams-live-validation.md)
are recorded. The conservative automated behavior is plain normal send and
route-scoped explicit quote only.

#### Reply-correlation live evidence

On 2026-08-07, a commercial-public-cloud Personal chat test confirmed that
`SendToConversation` with only a body `replyToId` returned `Delivered` with a
real activity ID but rendered as a plain message with no quote UI. The agent
output contained a valid route-scoped `[[reply_to:...]]` directive, and Gateway
reported neither target fallback nor rejection. OpenAB therefore switched
explicit quotes to the documented `ReplyToActivity` endpoint.

A follow-up test against that endpoint also returned `Delivered` with a real
activity ID, with no target fallback, rejection, or unknown outcome. The same
Personal client still rendered a plain message. For the tested commercial
tenant/client, Personal normal responses and explicit bot replies therefore
have no visual quote treatment; the explicit path remains useful as correct
Bot Connector correlation and for the still-unverified group/channel scopes.

### Bot-owned update and delete

- **Decision:** [Teams Owned Message Mutations](adr/teams-owned-message-mutations.md)

Automated mocks and WebSocket tests cover URL construction, ownership,
structured outcomes, retry bounds, ordering locks, and rolling compatibility.
Before the proposed Teams delivery stack is described as complete, the
corresponding cases in the [current tracker](msteams-live-validation.md) must be
recorded.

Verified on 2026-08-07 in a commercial-cloud Personal chat:

- a send returned `Delivered` with a non-empty activity ID;
- updating that bot-owned activity returned `Delivered` and changed the same
  Teams message in place;
- deleting it returned `Delivered` and removed it from the Teams client;
- an edit after the delivered delete failed closed as `Rejected /
  message_not_owned`, without entering Connector mutation transport;
- after an actual Gateway process restart, a command carrying the old origin
  and target failed closed as `Rejected / target_origin_not_found`;
- the probe remained the only WebSocket consumer, produced no `Unknown`
  outcomes, and restored the normal Core consumer after completion.

This evidence closes only the Personal happy path and the two listed
process-local rejection cases. The remaining acceptance set must still confirm:

- update and delete of bot-owned group-chat and channel activities;
- rejection behavior for malformed or externally deleted IDs, plus route TTL
  and capacity eviction;
- live `429 Retry-After` behavior;
- channel reply-chain presentation after update;
- send/update/delete behavior across app uninstall and reinstall.

### Public-preview reactions

- **Decision:** [Teams Message Reactions Preview](adr/teams-message-reactions-preview.md)

Enable the setting, restart both peers, and verify personal, group-chat, and
channel-mention turns. The inbound user message should receive status reactions
and Gateway logs should include `gateway → teams reaction`.

Verified on 2026-08-07 in a commercial-cloud Personal chat:

- the tested tenant exposed the public-preview Connector reaction operations;
- add and remove against an authenticated inbound activity both returned
  `Delivered` and were visible in the Teams client;
- the normal status lifecycle retained the queued `👀` receipt, removed the
  temporary `🤔`, and completed with `🆗` plus a mood reaction;
- the `❌` error mapping returned `Delivered`, was visible, and was removed
  without residue;
- a reaction against a bot-owned activity after its delivered delete failed
  closed as `Rejected / reaction_target_not_known`, without entering Connector
  reaction transport;
- after an actual Gateway process restart, a reaction carrying the old origin
  and inbound activity failed closed as `Rejected / target_origin_not_found`
  and did not reappear in the client;
- with a temporary five-second route TTL, add/remove succeeded before expiry and
  a later add failed closed as `Rejected / target_origin_not_found`; the
  original deployment configuration was restored byte-for-byte afterward;
- the probes remained the only WebSocket consumer and produced no `Unknown`
  outcomes.

A separately capped three-write burst returned three `Delivered` outcomes and
emitted no bounded-retry warning, so the tested tenant did not return `429` in
that run. Cleanup removed all test reactions. This is `NOT OBSERVED`, not a
`429 Retry-After` pass; the probe was not escalated beyond three writes.

`NEW EVIDENCE` from a 2026-08-09 Personal ACK-loss probe exercised both stall
timers. The deployed pre-fix Gateway returned four
`Rejected / unsupported_reaction` outcomes: soft-stall `🥱` add, hard-stall
`😨` add plus old `🥱` remove, and old `😨` remove during the final error
swap. The final UI showed `👀` plus the successfully added `😱`; content
ambiguity handling remained independent.

A follow-up Personal turn closed this mapping defect on 2026-08-09. The literal
`PR7-STALL-REACTIONS-OK` string below is a sanitized probe marker, not an
upstream pull-request number. Only
Gateway was replaced with the clean `620f323` image; Core and Tunnel remained
unchanged. Relative to the permanent queued add, Gateway recorded thinking at
+6.507 seconds, soft-stall add at +15.852 and prior-state remove at +16.348,
hard-stall add at +35.809 and soft-stall remove at +36.325, then done add at
+42.494, hard-stall remove at +43.002, and mood add at +43.489. The six adds
and three removes produced no `unsupported_reaction`, other rejection,
`Unknown`, `429`, no-consumer outcome, WebSocket reconnect, or error. The
operator observed both `🥱` and `😨`; the completed UI retained only `👀`,
`🆗`, and the mood reaction. It showed one `Edited` bot activity containing
`[01]` through `[10]` and `PR7-STALL-REACTIONS-OK`, with no duplicate,
warning, or extra placeholder. Core and Gateway remained healthy with one
direct Core WebSocket consumer.

This evidence closes only the tested Personal lifecycle and rejection cases.
The remaining acceptance set must still record:

- add and remove against inbound group-chat and channel activities;
- mappings not exercised by the Personal lifecycle, including tool
  transitions;
- replacement ordering during tool transitions;
- the documented two-reactions-per-second throttle and live
  `429 Retry-After` behavior;
- stale/deleted rejection in the unavailable scopes;
- cross-tenant/cross-conversation rejection;
- behavior when the tenant has not received the public-preview rollout.

Automated mocks prove local routing and transport behavior but cannot close the
Microsoft public-preview availability gate.

### Typed scope and mention routing

- **Decision:** [Teams Typed Scope And Mention Routing](adr/teams-typed-scope-and-mention-routing.md)

#### Typed-scope acceptance record

The 2026-08-07 Microsoft 365 Personal and rolling-upgrade subset passed with
the `cf0fe4a` images:

- new Gateway → old Core accepted one Personal activity and returned one normal
  agent response;
- new Gateway → new Core, with `allow_personal = true` and the authenticated
  sender in `allowed_users`, accepted a mention-free Personal activity through
  typed scope without a missing-scope fallback, scope deny, identity deny, or
  delivery failure;
- a scope-only `[teams]` configuration correctly failed closed at L3 and
  returned the request-access echo until the sender was explicitly admitted;
- old Gateway → new Core logged one missing-scope compatibility fallback,
  accepted the same admitted identity, and returned the requested agent
  response; and
- every transition preserved one active WebSocket consumer and recreated only
  the service under test.

GroupChat, Team channel root, and Team channel reply environments remain
unavailable. Those cases are `SKIPPED`, never `PASS`, and still gate complete
typed-scope live acceptance.

#### Typed-scope implementation-status summary

Microsoft 365 Personal and rolling compatibility are live-validated as
described above. GroupChat, Team channel root, and Team channel reply remain
`SKIPPED` for lack of an environment, so complete typed-scope live acceptance
remains open.

### Processing-message lifecycle

- **Decision:** [Teams Processing Indicator](adr/teams-processing-indicator.md)

#### Processing-lifecycle acceptance record

Live Microsoft 365 acceptance requires Personal, groupChat, Team channel root,
and Team channel reply environments. Verify visible create/update/terminal/
delete ordering, a tool transition, error/timeout behavior, reaction-preview
coexistence, and cleanup failure. Missing group/channel environments remain
`SKIPPED`, never `PASS`.

The 2026-08-08 Microsoft 365 Personal success subset passed with clean
`e6aefb7` Gateway and Claude images:

- one admitted Personal prompt requested a read-only tool action and an exact
  final response;
- the operator confirmed one visible processing activity completed and cleared,
  the exact requested final response arrived, and the queued `👀` receipt
  remained on the inbound activity;
- Gateway recorded, in order, one reaction add, one status POST, one terminal
  PUT, one final-content POST, and one status DELETE;
- no typed-scope fallback, scope or identity deny, processing-status failure,
  delivery failure, invalid-token rejection, invalid event, or multi-consumer
  error was recorded; and
- Gateway and Core remained healthy with matching non-empty WebSocket tokens
  and `active_consumers = 1`.

Only one PUT was recorded, so this run proves the terminal update but not a
distinct intermediate tool-label update. Personal tool-transition,
error/timeout, and cleanup-failure cases remain open. GroupChat, Team channel
root, and Team channel reply remain `SKIPPED` for lack of environments.

#### Processing-lifecycle implementation-status summary

Microsoft 365 Personal success ordering and reaction coexistence are
live-validated as bounded above. Personal tool-transition, error/timeout, and
cleanup-failure evidence remains open; groupChat and Team channel root/reply
remain `SKIPPED` until environments exist.

### Progressive content

- **Decision:** [Teams Progressive Response](adr/teams-progressive-response.md)

Literal `PR7-*` strings in this section are sanitized probe markers, not
upstream pull-request numbers.

A rolling deployment paired the clean `31999aa` Claude Core image with the
already-live `e6aefb7` Gateway. The processing-message and reaction-preview
opt-ins remained enabled for the first five Personal probes. The later
reaction-progress, explicit-reply, route-expiry, and ACK-loss probes set
`processing_indicator = "off"` while preserving streaming and reaction preview.
Core and Gateway were healthy with matching non-empty WebSocket tokens at each
verification checkpoint, with one Core WebSocket consumer before and after all
ten Personal probes.

The Personal progressive-success case passed:

- one authenticated read-only-tool prompt created one processing activity and
  one real content placeholder;
- Gateway recorded one queued-reaction add, two activity POSTs, eighteen PUTs,
  and one status DELETE, with no extra final POST;
- the stable content-edit portion advanced at roughly 1.95–2.06-second
  intervals, consistent with the 1.5-second changed-display coalescing floor;
- the operator observed one Teams activity marked `Edited`, the requested tool
  result and complete answer in that same activity, and the exact terminal
  marker `PR7-PERSONAL-OK`; and
- after completion the processing activity was gone while the inbound `👀`
  receipt remained.

The Personal single-overflow case also passed:

- one authenticated long-form prompt produced one processing activity, one
  progressively edited placeholder, and exactly one overflow activity;
- Gateway recorded one queued-reaction add, three activity POSTs in total,
  fifty-three PUTs, and one status DELETE; forty-nine of fifty-two successive
  aggregate edit intervals were between 1.5 and 3 seconds;
- the final transport sequence was two PUTs, the overflow POST, then status
  DELETE, with no rejected, unknown, failed, warning, `429`, or reaction-remove
  outcome; and
- the operator observed exactly two ordered content activities: `[01]` through
  `[15]` in the edited placeholder, then `[16]` through `[22]` and
  `PR7-OVERFLOW-OK` in the overflow activity. No marker was missing, duplicated,
  or reordered, the processing activity cleared, and `👀` remained.

The Personal multiple-overflow case passed:

- one authenticated 36-section prompt produced one progressively edited
  placeholder followed by exactly three overflow activities;
- Gateway recorded one queued-reaction add, five activity POSTs in total, one
  hundred ten PUTs, and one status DELETE; five real activity IDs were recorded,
  and 107 of 109 successive aggregate edit intervals were between 1.5 and 3
  seconds;
- the final transport sequence was two PUTs, three sequential overflow POSTs,
  then status DELETE, with no rejected, unknown, failed, warning, `429`, or
  reaction-remove outcome; and
- the operator confirmed exactly four ordered content activities containing
  every marker from `[01]` through `[36]` without omission, duplication, or
  reordering, followed by `PR7-MULTI-OVERFLOW-OK`. The processing activity
  cleared and `👀` remained.

The Personal Gateway-restart boundary also passed:

- after one processing POST, one placeholder POST, and twenty-eight PUTs, the
  existing Gateway container was intentionally restarted while the turn was
  still streaming;
- for more than one minute after restart, Gateway received no further write
  command, created no activity ID, and sent no fresh answer or warning activity;
- the operator observed the expected non-durable boundary: the processing
  activity remained, the edited placeholder stopped partway through `[14]`,
  `PR7-ROUTE-EXPIRY-OK` never appeared in a bot response, no duplicate or
  warning appeared, and the inbound `👀` remained; and
- Gateway recovered healthy with exactly one Core WebSocket consumer while
  Core and Tunnel remained running. Compose and config hashes were unchanged.

A fresh Personal event after restart proved new-route recovery:

- Gateway recorded one queued-reaction add, two activity POSTs, two PUTs, and
  one status DELETE, with no rejected, unknown, failed, warning, or `429`
  outcome; and
- the operator observed exactly `PR7-POST-RESTART-OK`, the new processing
  activity cleared, the new `👀` remained, and both old orphaned activities were
  left untouched.

A Personal reaction-progress-with-streaming case passed after disabling the
processing-message backend:

- the isolated turn recorded seven reaction adds, four reaction removes, one
  content POST, four content PUTs, and no DELETE, warning, error, rejected,
  unknown, delivery-failure, or `429` outcome;
- the operator observed the permanent queued receipt, transient status
  reactions, `🆗` plus a mood reaction, and `REACTION-PROGRESS-OK` in the same
  edited content activity; and
- no separate `Processing…` activity appeared. The rollout window contained two
  admitted events, so these counts apply only to the later isolated turn.

The Personal explicit-reply case also passed:

- one newly admitted turn recorded four reaction adds, one reaction remove, two
  activity POSTs, one placeholder PUT, and one placeholder DELETE;
- transport ordering was placeholder POST, cosmetic PUT, quoted final POST, then
  placeholder DELETE. The quoted POST was acknowledged before deletion, and no
  quote-target fallback, rejected, unknown, warning, delivery failure, or `429`
  occurred; and
- the operator observed only `PR7-EXPLICIT-REPLY-OK`; the placeholder was gone,
  no duplicate or processing activity remained, and the Personal client rendered
  the official reply transport as a plain message without visual quote chrome.

A bounded Personal route-expired resumed-finalization case passed:

- after an explicit cleanup turn, only Gateway was recreated with a temporary
  two-second route TTL. One no-tool long-form turn admitted exactly once and
  created its real placeholder at +1.102 seconds, before route expiry;
- Gateway returned `Rejected / target_origin_not_found` for cosmetic PUTs at
  +3.039, +4.542, and +6.045 seconds, then for the final PUT at +50.353 and
  recovery DELETE at +50.354. The one fresh-answer POST at +50.355 and the
  subsequent router-warning POST at +50.357 both returned
  `Rejected / route_not_found`;
- no actual Microsoft write occurred after the two-second boundary, and no
  `Unknown`, `429`, or no-consumer outcome occurred. The operator observed only
  the original `...` placeholder: `PR7-ROUTE-EXPIRED-TYPED-OK`, fresh content,
  duplicates, warning activities, and completion reactions were all absent; and
- the orphan was left untouched as required by process-local ownership. The
  environment was restored byte-for-byte to the default 3,600-second TTL by
  recreating only Gateway; Core and Tunnel stayed unchanged, both services were
  healthy, and exactly one Core WebSocket consumer remained.

A bounded Personal final-content ACK-loss case also passed:

- a standard-library WebSocket proxy bound only to the Docker bridge forwarded
  normal traffic and the selected marker-bearing PUT to the unchanged Gateway,
  but dropped its matching ACK. It persisted no token, request ID, channel ID,
  or content; client and Gateway hello negotiation reported one supported
  consumer;
- exactly one admitted no-tool turn created its placeholder at +6.049 seconds
  and made thirteen content edits. The target edit was observed at +35.956, its
  Microsoft PUT completed at +35.959, and the proxy dropped the corresponding
  `Delivered` ACK at +36.846;
- after the selected write, both proxy state and Gateway logs showed zero
  additional content command or transport operation: no retry PUT, DELETE,
  fresh send, or warning send occurred. There was no content rejection, `429`,
  no-consumer failure, or WebSocket reconnect; and
- the operator observed exactly one `Edited` bot activity containing `[01]`
  through `[06]` and `PR7-UNKNOWN-ACK-DROP-OK`, with no duplicate, fresh answer,
  warning activity, or extra placeholder. Four independent stall-reaction
  mapping rejections were recorded as `NEW EVIDENCE`; a later Personal rollout
  verified both corrected mappings without rejection. Neither result changes
  this content outcome. The original config was restored byte-for-byte,
  the proxy and backup were removed, Core returned to a direct connection, and
  Core and Gateway were healthy with one consumer.

A bounded Personal placeholder-POST ACK-loss case also passed:

- a standard-library bridge-only proxy matched only Core's fixed placeholder
  payloads and claimed the target process-wide before forwarding it. It did not
  persist token, request ID, channel ID, or content;
- exactly one admitted no-tool turn recorded the queued reaction at +0.000
  seconds, the target placeholder command at +6.625, the Microsoft content POST
  at +6.688, and the dropped corresponding `Delivered` ACK at +7.409;
- during a 105.810-second post-drop observation, proxy and Gateway evidence
  showed exactly one content POST, zero PUT or DELETE, and no later content
  command, fresh send, or warning send. Five post-target status-reaction
  commands remained independent. There was no Connector rejection, reaction
  rejection, `429`, no-consumer failure, WebSocket reconnect, or process error;
- the operator screenshot showed exactly one bot placeholder rendered as `...`.
  The requested final answer was absent from bot activities, no duplicate,
  warning, or extra placeholder appeared, and only permanent `👀 + 😱`
  remained after the transient reactions cleared; and
- the original config was restored byte-for-byte by recreating only Core.
  Gateway and Tunnel remained unchanged, the proxy and backup were removed,
  both services were healthy, and the direct Core WebSocket consumer returned
  to one.

These observations close only the Personal success, one-placeholder,
coalescing, same-activity finalization, processing/reaction coexistence,
reaction-progress/streaming coexistence, explicit-reply delivered-before-delete,
single- and multiple-overflow delivery, restart-orphan, no-post-restart-write,
fresh-route recovery, explicit Rejected final-PUT/delete/recovery-send
route-expiry, final-content Delivered-ACK-loss Unknown/no-retry, and
placeholder-POST Delivered-ACK-loss Unknown/no-retry subsets. The restart case
itself issued no post-restart write, while the separate bounded TTL and two
ACK-drop injections exercised explicit Rejected and two distinct Unknown
branches. Recovery-DELETE/POST, explicit-reply, and overflow `Unknown` branches,
other cleanup-failure outcomes, observable `429 Retry-After`, and a
production-length turn near the default TTL remain open. GroupChat, Team channel
root, and Team channel reply remain `SKIPPED` because no environment is
available.

### Graph-free attachments

- **Decision:** [Teams Attachment Ingress](adr/teams-attachment-ingress.md)

Live acceptance remains separate. Personal must verify inline image, image file,
UTF-8 text file, attachment-only input, rejected metadata, text-plus-attachment,
and default-off behavior. GroupChat and channel inline-image cases remain
`SKIPPED` when environments are unavailable; paperclip files outside Personal
remain unsupported without Graph.

#### Attachment live evidence

- `[LIVE VERIFIED 2026-08-10 — Personal inline image with text]` Canonical
  `9bf9d7f` Standalone Core and Gateway, with attachment ingress enabled on both,
  processed one operator-approved Personal activity containing prompt text and a
  pasted inline PNG. Gateway observed exactly one post-gate
  `materialize_attachment` command, one content send, one content edit, four
  reaction adds, and one reaction removal. There was no attachment, write,
  hello, topology, reconnect, or process error and no late operation after the
  declared 180-second window.
- Operator UI evidence showed one `Edited` bot activity with the exact code that
  appeared only in the image, no duplicate or warning activity, and the queued
  `👀` receipt retained.
- `[LIVE VERIFIED 2026-08-10 — Personal attachment-only inline image]` A second
  operator-approved Personal activity contained one pasted inline PNG and no
  text or caption. It again produced exactly one post-gate
  `materialize_attachment` command, one content send, one content edit, four
  reaction adds, and one removal. There was no attachment, write, hello,
  topology, reconnect, process, or late-operation error. Operator UI evidence
  showed one `Edited` bot activity with the exact image-only code, no duplicate
  or warning activity, and the queued `👀` receipt retained.
- The screenshots were inspected in place and are not stored in the repository
  or memory. Neither turn emitted the Core `batch dispatched` info record, so
  that log-only signal remains `NOT OBSERVED`; it is not substituted for the
  independent transport and UI evidence.
- `[LIVE PENDING]` Personal paperclip image and UTF-8 text files require the
  separately approved `supportsFiles = true` profile. Personal rejected-metadata
  and default-off cases remain open. GroupChat and channel inline-image cases
  remain `SKIPPED` until environments are available.

### Formatting and long messages

- **Decision:** [Teams Formatting And Long Messages](adr/teams-formatting-and-long-messages.md)

#### Recorded Personal evidence

A controlled Microsoft 365 Personal subset subsequently passed against canonical
`linux/amd64` Core and Gateway binaries for the implementation commit. One
table event rendered the default aligned fenced-code fallback in a single
activity. A deterministic,
offline ACP fixture then supplied an 82,020-UTF-16-byte fenced response; Teams
showed three ordered, balanced code-block activities with the ASCII body,
BMP-plus-supplementary tail, and permanent queued receipt intact. The fixture
changed only the ACP response source; the canonical OpenAB binary hash remained
unchanged.

A separate one-shot bridge probe targeted the middle overflow POST of a
162,038-UTF-16-byte three-chunk fixture. Microsoft delivered the target chunk,
its Delivered ACK alone was withheld from Core, and the following observation
window contained zero content commands: no suffix POST, retry, DELETE, cleanup,
or warning activity. Teams UI showed only the delivered prefix and target
marker; the suffix and end markers were absent. The direct Gateway connection
was then restored byte-for-byte, with Gateway and Tunnel containers unchanged
and all temporary proxy artifacts removed. Runtime filtering hid the Core
Unknown and `batch dispatched` records, so those log-only markers remain
`NOT OBSERVED`; the independent proxy, transport, timeout, and UI evidence
establishes the exercised Unknown branch.

A final deterministic Personal fixture supplied 239,964 UTF-16 bytes spanning
ASCII, BMP, and supplementary-plane emoji. Core produced four budget-compliant
content chunks. The transport sequence was placeholder POST, final PUT, one
delivered overflow POST, one rejected overflow POST, and the existing single
Rejected-warning POST. Microsoft explicitly rejected zero-based chunk 2 as
`message_too_large`; Core reported 2 of 4 chunks delivered, did not send chunk
3, and made no retry, DELETE, or cleanup write during a 592-second quiet window.
Teams UI showed the session-reset notice, ASCII begin/end markers, the permanent
queued receipt, and the accurate partial-delivery warning. The BMP and emoji end
markers were absent, as required after the rejected middle chunk.

This direct 413 closes the explicit-Rejected branch and confirms ordered
stop-on-Rejected behavior plus sanitized partial-delivery reporting. It does
not prove successful pure-BMP or pure-emoji near-limit delivery. Exact BMP and
supplementary-scalar accounting remains covered by deterministic automated
tests. Because this proposal records the residual risk that the 80,000-byte
target may receive a 413 and directs maintainers to revisit headroom after
direct evidence, successful single-unit live boundaries are a focused headroom
follow-up rather than a new acceptance blocker.

The recorded Personal evidence covers table fallback, mixed-Unicode
long fenced code, ordered overflow, intermediate ACK-loss `Unknown`, and an
explicit intermediate `Rejected`. GroupChat and channel remain `SKIPPED` until
suitable environments exist.

#### Acceptance-status summary

Microsoft 365 Personal evidence verifies table fallback, mixed
ASCII/BMP/emoji long fenced-code presentation, ordered multi-chunk display,
intermediate overflow ACK-loss `Unknown`, and explicit
`message_too_large` rejection with no later chunk. Successful near-limit
pure-BMP and pure-emoji delivery is retained as a headroom follow-up because
the direct 413 is a documented residual risk, not an unrecorded failure mode.
GroupChat and channel remain `SKIPPED` until environments exist. Build,
deployment, proxy injection, and every future live event each require
separate authorization.

### Text commands

- **Decision:** [Teams Text Command Parity](adr/teams-text-command-parity.md)

#### Text-command Microsoft 365 record

Bounded operator-initiated Personal events exercised the deployed canonical
`3ba76145b12fe3c22b4294999ecd4e6df6daeb54` Core and Gateway without changing
the tenant app package. Evidence contains only sanitized counts and in-place UI
judgments; screenshots, usage
values, sender/conversation/activity IDs, URLs, and message bodies were not
persisted.

- `[LIVE VERIFIED — query commands and compatibility]` `/models`, `/agents`,
  `/model list`, and `/agent list` each produced one ordinary command response.
  Teams displayed five model options or six agent options with exactly one
  current marker. Each isolated window had zero content mutations, command
  reactions, agent spawns, session creations, attachment materializations,
  `Rejected`, or `Unknown` outcomes, and the existing ACP process count did not
  increase.
- `[LIVE VERIFIED — private usage branches]` Authenticated typed Personal
  `/usage` first reached the command path against the canonical Claude backend
  and returned one bounded unsupported-backend response without exposing or
  persisting usage values or reaching the agent. A temporary deterministic
  supported-backend fixture then reused the byte-identical canonical Core binary
  and synthetic data only. One setup event produced one content send and one
  final mutation; its content-free audit recorded one initialize, session-new,
  and prompt operation. The isolated `/usage` event issued exactly one backend
  query and one ordinary content send, with zero prompt or session delta,
  mutation, reaction, attachment materialization, `Rejected`, or `Unknown`.
  Teams showed one user activity and one bot activity containing the usage
  heading, one bounded breakdown, a progress indicator, and a billing-reset
  line, with no duplicate or warning. Displayed values and the in-place
  screenshot were not persisted.
- `[LIVE VERIFIED — fixture restoration]` The fixture rollout recreated Core
  only. After the supported `/usage` event, byte-exact compose, environment, and
  config restoration recreated canonical Claude Core only; Gateway and Tunnel
  identities remained unchanged. Health, zero restarts, equal non-empty tokens,
  one direct Core consumer, zero ACP processes, and a 60-second zero-event and
  zero-write soak all passed. Fixture artifacts and rollback backups were
  retained; no registry push or cleanup was performed.
- `[LIVE VERIFIED — reset]` `/reset` returned one ordinary acknowledgement,
  removed the active session, and reduced the ACP process count from one to
  zero without creating another session or triggering command progress.
- `[LIVE VERIFIED — in-flight cancel]` A two-event probe placed `/cancel` 0.933
  seconds after the first progressive mutation of an active turn. Teams retained
  only a partial response, delivered one cancel acknowledgement, performed one
  finalizing mutation 0.500 seconds later, and emitted no further writes during
  a 30-second quiet window. The session remained active.
- `[LIVE VERIFIED — buffered cancel-all]` Under the deployed default
  `per-message` mode with buffer capacity one, a corrected three-event probe
  held one sentinel message pending for 6.158 seconds. `/cancel-all` reported
  both cancellation and buffer clearing; the sentinel produced no bot response
  or late execution, and the following 30-second quiet window was stable. An
  earlier attempt that waited for the sentinel's `👀` was classified
  `NOT EXERCISED`, because that receipt is added only after dequeue; it is not
  counted as product evidence or a product failure.
- Across the successful command probes, services stayed healthy with zero
  restarts and one direct Core consumer. No duplicate command response,
  attachment work, or command-side progressive lifecycle was observed. Runtime
  filtering hid the content-free `gateway command completed` records, so those
  markers remain `NOT OBSERVED`; independent transport ordering, process counts,
  required-ACK outcomes, and UI evidence establish the verdicts.
- Personal scope is mention-free by design. Structured recipient-mention cleanup
  still requires GroupChat or Team channel, both unavailable here. Command-menu
  discovery and scope separation also remain `SKIPPED` because the installed app
  package could not be upgraded.

#### Deployment and status summary

Canonical `linux/amd64` Core and Gateway images from implementation commit
`3ba76145b12fe3c22b4294999ecd4e6df6daeb54` were deployed Core-first and remained
healthy through the Personal evidence above. A temporary Core-only fixture
closed the supported-backend `/usage` branch and was then restored byte-exact to
canonical Claude Core. The tenant package was not changed, and no push or PR
creation was performed. Personal command runtime evidence is recorded; the
unavailable menu, structured-mention, GroupChat, and Team channel cases remain
explicitly `SKIPPED` rather than passed.

### Persistent conversation registry

- **Decision:** [Teams Trusted Persistent Conversation Registry](adr/teams-trusted-persistent-conversation-registry.md)

#### Registry acceptance procedure

A separately authorized Microsoft 365 acceptance may enable a temporary
registry path, send one trusted Personal activity, verify a sanitized active
count, recreate Gateway only, verify the same generation reloads, then restore
byte-exact configuration. GroupChat/channel, installation removal, and blocked
403 remain `SKIPPED` unless safe environments and explicit destructive-test
authorization exist. Raw records and identifiers must not be retained as live
evidence.

#### Registry Microsoft 365 record

`[LIVE VERIFIED 2026-08-20 — Personal promotion ordering and restart
persistence]` The first bounded Personal command probe against implementation
`00e5162574f0bd70c0f240b66cf15db81acd54f7` exposed a Standalone scheduling
race: the one content write preceded the registration request. That probe was
classified FAIL and the canonical environment automatically rolled back. Fix
`5daf151123176eeda547b9de6decfa06def517fd` moved every Standalone Teams event
onto the detached event processor, added task reaping and a blocking ordering
regression test, and passed the complete automated matrix again.

After the Core-first fix rollout, the retained generation-1 active record loaded
across a Gateway-only recreation. One independently authenticated Personal
command then produced exactly `trusted inbound → registration request →
registry commit → content request → content write`; there was one refresh, one
content write, and zero mutation, reaction, task failure, `Rejected`, `Unknown`,
socket error, or agent process. The closed schema, complete composite key,
active count, generation transition to 2, `0600` file, `0700` directory, valid
Gateway-local endpoint and absence of forbidden fields all passed sanitized
inspection. UI contained one user activity and one command response with no
duplicate, placeholder, or extra activity; the screenshot and displayed text
were inspected in place and not retained. A final Gateway-only recreation
reloaded the same generation-2 active record while Core and Tunnel stayed
unchanged; health, restart count 0, equal non-empty tokens, direct consumer 1,
and a 30-second zero-event/zero-write soak passed. The validated registry
deployment remains enabled through an explicit mode-`0600` override while base compose,
environment and config files remain byte-identical. GroupChat/channel,
installation removal, and blocked-403 live cases remain `SKIPPED` for unavailable
or destructive environments.

### Operator cron delivery

- **Decision:** [Teams Operator Cron Delivery](adr/teams-operator-cron-delivery.md)

#### Operator-cron acceptance procedure

A separately authorized Microsoft 365 acceptance may reuse the existing trusted
Personal record, restart Gateway before the scheduled time, enable one temporary
operator baseline job, and verify:

- no new inbound activity or registration is required;
- exact persistent lookup precedes one trigger content write and all ACP work;
- UI shows one scheduled trigger and one agent response with no duplicate,
  placeholder, or extra activity;
- the registry remains one active record and no identifier or message content is
  retained as evidence;
- Core/Gateway health, restart counts, token equality, direct-consumer count,
  and a post-test quiet soak pass; and
- temporary schedule/config changes are restored byte-exact while accepted
  binaries may remain deployed.

GroupChat/channel UI, destructive uninstall, live blocked-403, TTL-boundary,
and token-rotation cases remain `SKIPPED` unless safe environments and explicit
additional authorization exist.

#### Operator-cron Microsoft 365 record

- Clean-context `linux/amd64` Core and Gateway images for the source commit were
  archive-hash verified, removed, reloaded, and checked by image revision and
  binary hash without a registry push. Core-first rollout against the validated
  registry-only Gateway fired one bounded synthetic Teams tick, rejected the
  missing persistent-send capability, and produced zero Gateway writes, inbound events,
  agent spawns, or sessions.
- After Gateway rollout, one Gateway-only restart reloaded the unchanged
  generation-20 registry for a second time while Core and Tunnel remained
  unchanged. Both rollout quiet soaks had zero inbound event, content write,
  mutation, reaction, cron fire, or socket error; exactly one direct Standalone
  consumer remained.
- One temporary operator baseline job then used the exact active Personal route.
  It fired once and produced exactly two content writes: the first trigger
  returned the required real platform identifier before one agent spawn and one
  session resume, followed by one agent response write. There were zero inbound
  events, capability rejections, trigger-delivery failures, cron errors, or
  blind retries. Five status-reaction operations and one later bot-owned
  mutation were turn-local operations rather than extra content activities.
- The first harness verdict was a false negative because it treated status
  reactions as forbidden extra activities and its remote Python runtime could
  not parse Docker's nine-digit fractional timestamps. The harness immediately
  restored the temporary config and did not rerun the user-visible schedule.
  Required-ACK control flow, sanitized counts, and the final UI structure jointly
  establish `cron fire → trigger delivered → agent work → response write`.
- Personal UI contained exactly one scheduled trigger and one agent response,
  with zero duplicate, placeholder, or extra content activity. One edit marker
  matched the bot-owned mutation and one status reaction remained visible; the
  response's semantics were not an acceptance criterion. The supplied snapshot
  and displayed text were inspected in place and were not copied or retained.
- Final sanitized inspection found one active generation-20 record with zero
  consecutive forbidden outcomes. Base compose, environment, and config files
  were byte-identical; the temporary job was absent; Core retained no Gateway
  Teams credentials; Core and Gateway were healthy with restart count zero,
  equal non-empty transport tokens, and one direct consumer. A final read-only
  20-second soak had zero inbound event, write, mutation, or reaction.
- Sanitized evidence SHA-256:
  `ec78af676b187bc62d3a16c5a6d1857a6284fe3a91df5e68f2f8e0adc3e0193e`.
  It contains no private Teams identifier, message text, platform write
  identifier, endpoint value, raw log, absolute timestamp, or snapshot copy.
  GroupChat/channel UI, destructive uninstall, live blocked-403, TTL-boundary,
  and token-rotation cases remain `SKIPPED` exactly as scoped.

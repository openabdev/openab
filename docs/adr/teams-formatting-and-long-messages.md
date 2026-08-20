# ADR: Teams Budget-Aware Formatting and Ordered Long Messages

- **Status:** Proposed
- **Date:** 2026-08-10
- **Author:** @NeoHsu
- **Related:**
  - [Gateway capabilities and delivery semantics](gateway-capabilities-and-delivery-semantics.md)
  - [Teams real send acknowledgement](teams-real-send-acknowledgement.md)
  - [Teams progressive edit response](teams-progressive-response.md)
  - [Multi-platform adapters](multi-platform-adapters.md)

---

## Context

At the initial long-message design baseline, OpenAB advertised Teams with a conservative
4,096-character message limit. Core converted every negotiated non-character
`MessageLimit` into an even smaller character count before calling
`format::split_message`. This was a safe
compatibility default, but it does not model the Teams platform contract:

- Microsoft documents an approximate 100 KB agent-message limit;
- the size includes message text, image links, mentions, and reactions encoded
  as UTF-16, but excludes base64-encoded images;
- Microsoft recommends keeping the message itself within 80 KB for reliable
  delivery; and
- an oversized message returns HTTP 413 with `MessageSizeTooBig`.

The existing capability schema already has additive `characters`, `bytes`,
`utf16_bytes`, and `unlimited` variants. The missing work is exact budget-aware
splitting and a Teams capability value that reflects the documented unit.

Teams receives OpenAB content as text-only Bot Framework activities with
`textFormat = markdown`. Microsoft documents only a subset of Markdown and
states that text-only messages do not support tables. Rich cards likewise do
not add Markdown-table support. OpenAB already has a table pre-pass whose default `code` mode converts a
Markdown table to an aligned fenced block before message splitting, but at that
baseline the Teams setup guide recommended bypassing the fallback with
`tables = "off"`.

Long-message delivery also has two different safety levels. Structured Teams
progressive finalization already waits for each required ACK, sends overflow in
order, and stops on the first rejected or unknown write. The ordinary
send-once branch awaits each `Result`, but continues with later chunks after an
earlier failure. That can show a suffix without the missing middle and does not
report a precise partial-delivery boundary.

## Decision

### 1. Teams advertises an exact UTF-16 byte budget

The Teams capability in both Standalone Gateway hello and Unified mode will be:

```text
MessageLimit::Utf16Bytes { max: 80_000 }
```

`80_000` is deliberately decimal, not `80 * 1024`. It follows Microsoft's
recommended 80 KB implementation target rather than treating the approximate
100 KB rejection threshold as a guaranteed text allowance.

For this limit, Core measures a string as:

```text
utf16_bytes(text) = 2 * text.encode_utf16().count()
```

A BMP scalar therefore costs two bytes and a supplementary-plane scalar costs
four. The limit is not a Unicode-scalar, grapheme, UTF-8-byte, or display-column
count. Table rendering and final display composition happen before the budget
is applied, so their expanded output is included.

OpenAB's current Teams content activity does not emit outbound mention entities,
cards, or inline base64 images. If those fields are added later, their encoded
size must receive an explicit reserve or a lower text budget before they share
this capability. This proposal does not infer authenticated mentions from plain
text.

### 2. Core gains one budget-aware splitter

Keep `format::split_message(text, character_limit)` as the compatibility wrapper
for existing direct callers. Add an internal splitter driven by the negotiated
`MessageLimit` with these unit definitions:

| Limit | Measurement |
| --- | --- |
| `characters` | Unicode scalar count, preserving current behavior |
| `bytes` | UTF-8 byte length |
| `utf16_bytes` | UTF-16 code units multiplied by two |
| `unlimited` | One unchanged chunk |

The splitter must preserve the existing structural behavior:

- prefer newline boundaries, then whitespace boundaries;
- preserve valid UTF-8;
- keep an extended grapheme cluster together whenever that cluster fits in an
  empty chunk;
- if one grapheme is larger than the budget, fall back only to Unicode-scalar
  boundaries; and
- fail before any final-content write if even one scalar cannot fit the
  advertised non-zero budget.

Fenced code blocks remain balanced independently in every chunk. The complete
opener, including its language tag, is repeated after a split; synthetic close
and reopen markers are included in the selected unit's budget. A zero limit or
an otherwise unsplittable non-empty value is an error, not an infinite loop,
truncation, invalid UTF-8, or oversized final-content write. A streaming turn
may already own its acknowledged placeholder; that existing activity follows
the [progressive-response failure lifecycle](teams-progressive-response.md) and
is never converted into a blind fresh send.

The 1.5-second cosmetic Teams edit path may retain its existing conservative
character preview. Authoritative final PUT/POST chunks use the exact negotiated
budget. This keeps intermediate writes safely below the limit without widening
this proposal into a streaming-preview redesign.

### 3. Markdown remains text-only and tables use the existing fallback

Teams outbound activities continue to set `textFormat = markdown`. This
proposal does not create Adaptive Cards or rich cards.

OpenAB preserves the agent's Markdown source except for the existing configured
table conversion:

- `tables = "code"` remains the default and recommended Teams setting;
- `tables = "bullets"` remains an accessibility-oriented fallback;
- `tables = "off"` remains an explicit operator bypass, but raw Markdown tables
  are not claimed to render as tables in Teams; and
- Teams continues to report `renders_native_tables = false` in both deployment
  modes.

The Teams setup documentation will stop recommending `tables = "off"`.
Mention-looking text and Markdown links are not rewritten, dropped, or copied
into later chunks; an individual token longer than the whole budget may still
be split at a valid Unicode boundary. Existing Discord mention-footer
propagation is unchanged and is not applied to Teams. Headings, lists,
strikethrough, and other Markdown remain subject to Microsoft's published
desktop/iOS/Android support differences. This proposal does not rewrite them
into a new rich-message schema or claim identical presentation across clients.

### 4. Required-ACK chunk delivery is sequential and terminal

Core builds the complete final chunk list before the first platform write. When
the resolved capability requires send ACKs, every POST is delivered through an
outcome-preserving method and the sequence obeys:

1. Send chunk `N` only after chunk `N - 1` is `Delivered`.
2. A delivered POST requires a non-empty real activity ID.
3. Stop at the first `Rejected` or `Unknown`; never skip it and send a suffix.
4. Never retry a rejected or unknown POST in Core. In particular, the existing
   Teams rule that records a 429 `Retry-After` without retrying POST remains
   unchanged.
5. An `Unknown` terminal outcome suppresses retry, fresh-send, cleanup, and the
   router's warning activity because the selected chunk may have committed.
6. A `Rejected` terminal outcome may use the existing single warning route,
   because rejection is explicit, but it does not resume the chunk sequence.

The delivery result records, without message content or activity IDs:

- total chunk count;
- number known delivered;
- zero-based failed chunk index; and
- terminal outcome kind and sanitized code.

If at least one earlier chunk was delivered, the error is explicitly classified
as partial delivery. Already-delivered activities are not deleted or edited in
an attempted rollback. Processing status and reaction progress become failed,
and status is cleared only after every final chunk is delivered.

This rule applies to ordinary send-once, explicit-reply-first, progressive
overflow, and rejected-placeholder recovery paths when required send ACK is
available. Existing progressive helpers already implement the ordering and
ambiguity rules; this proposal unifies the send-once branch with that behavior
rather than adding a second Teams-specific retry loop.

### 5. Rolling compatibility remains fail closed

| Core | Gateway | Result |
| --- | --- | --- |
| old Core without hello | new Gateway | Legacy behavior; Gateway accepts the first reply and sends no unsolicited control requirement. |
| capability-aware old Core | new Gateway | Decodes the existing `utf16_bytes` variant and uses its conservative quarter-size character bound; still safe, but not exact. |
| new Core | old Gateway or no valid hello | Existing `characters = 4096` legacy limit and legacy delivery semantics. |
| new Core | new Gateway | Exact 80,000 UTF-16-byte split plus required-ACK ordered delivery. |
| new Unified | embedded Teams adapter | Same exact budget and delivery semantics as new↔new Standalone. |

A valid hello remains authoritative. Missing Teams capabilities in a valid hello
do not fall back optimistically. Protocol version stays v1 because the
`utf16_bytes` wire variant was already additive before this proposal.

### 6. Observability is content-free

One finalization summary may record platform, total chunks, delivered chunks,
failed index, outcome kind, and sanitized error code. It must not log message
content, activity IDs, conversation IDs, request IDs, URLs, tokens, or serialized
Connector bodies.

A Microsoft HTTP 413 remains `Rejected / message_too_large`. It is evidence that
the conservative target was insufficient for that activity shape, not a reason
to retry the same body or silently change the limit during the turn.

## Security and reliability boundaries

- Trust admission, route ownership, tenant/conversation checks, and write
  serialization are unchanged.
- No Graph, RSC, delegated token, Adaptive Card permission, or manifest
  permission is added.
- POST `Unknown` is never retried or converted into a fresh warning activity.
- Partial delivery is not transactional; delivered prefix activities may remain
  when a later chunk fails.
- The feature does not claim exactly-once delivery, crash replay, durable
  ownership, or multi-consumer work distribution.
- Microsoft commercial public cloud remains the only supported Teams profile.

## Acceptance criteria

Automated verification must cover:

- exact character, UTF-8-byte, UTF-16-byte, and unlimited measurements;
- BMP, CJK, supplementary emoji, combining marks, ZWJ sequences, and mixed text;
- every emitted chunk fitting its own budget, including synthetic code-fence
  close/reopen markers and language tags;
- newline/whitespace preference, order preservation, no truncation, valid UTF-8,
  and explicit unsplittable-budget failure;
- Teams Standalone hello and Unified parity advertising
  `utf16_bytes = 80_000`;
- new Core→old Gateway/no-hello 4,096-character fallback and decoding by a
  capability-aware old Core;
- table conversion before splitting, Teams native-table=false behavior, and the
  explicit `off` bypass;
- send-once, explicit reply, progressive overflow, and recovery chunk order;
- stop-on-first `Rejected` and stop-on-first `Unknown`, with no later chunk;
- delivered/total/failed-index partial-delivery classification;
- no warning, delete, retry, or fresh send after an unknown POST;
- HTTP 413 mapping to `message_too_large`; and
- no regressions in existing character-based Discord/Slack splitting or Teams
  progressive ambiguity tests.

## Consequences

### Positive

- Teams uses the platform's documented unit instead of a fictional character
  limit.
- ASCII and BMP-heavy long replies require fewer activities while emoji-heavy
  replies remain correctly bounded.
- Code fences and table fallbacks are budgeted after rendering.
- Users never receive a later suffix after a known missing middle chunk.
- Rolling upgrades remain safe without a protocol bump.

### Negative

- The generic splitter becomes more complex and must carry unit-aware tests.
- Larger individual activities may take longer to render or edit even though
  they remain within Microsoft's recommendation.
- Partial delivery cannot be made atomic with Bot Connector primitives.
- Cosmetic streaming previews remain more conservative than final messages.

## Alternatives rejected

1. **Keep 4,096 characters permanently.** Safe but knowingly misrepresents the
   platform, produces unnecessary activities, and leaves the UTF-16 capability
   unused.
2. **Advertise 100 KB.** Rejected because Microsoft labels it approximate and
   recommends an 80 KB implementation target.
3. **Use 80,000 Unicode scalars.** Rejected because supplementary characters
   consume twice the UTF-16 bytes of BMP characters.
4. **Use UTF-8 bytes.** Rejected because it is not the unit Microsoft documents
   for this limit.
5. **Split inside Gateway.** Rejected because Core owns final formatting,
   directive handling, ordered delivery health, and Unified/Standalone parity;
   Gateway-side splitting would hide per-chunk outcomes from Core.
6. **Continue after a rejected middle chunk.** Rejected because a suffix without
   its missing middle is a corrupt user view.
7. **Retry an unknown POST.** Rejected because it can duplicate an activity that
   Microsoft already committed.
8. **Use Adaptive Cards for every answer.** Rejected because cards have different
   formatting semantics, do not solve Markdown tables, and belong to a later
   explicitly reviewed feature.

## References

- [Microsoft: Format your agent messages](https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/format-your-bot-messages)
- [Microsoft: Update and delete messages sent from agent](https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/update-and-delete-bot-messages)
- [OpenAB platform schema: Teams](../platforms/schema/teams.toml)

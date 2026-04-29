# ADR: Batched Turn Packing for ACP session/prompt

- **Status:** Proposed
- **Date:** 2026-04-27
- **Author:** @brettchien
- **Tracking issues:** #580
- **Related:** RFC #580 (Turn-boundary message batching) — extracts the packing aspect (T1.4 / B1) into a standalone decision record

---

## 1. Context & Decision

RFC #580 introduces turn-boundary batching: messages arriving on a thread while an ACP turn is in flight accumulate in a per-thread `mpsc::channel`, and the consumer drains them as **one** ACP `session/prompt` containing N concatenated arrival events. This ADR records how those N arrival events are **packed** into the `Vec<ContentBlock>` that crosses the broker → ACP boundary.

The original RFC MVP packing wrapped each sub-message in a new `<message index=N from="…">` tag and flattened all `extra_blocks` (images, transcripts) into a single tail of the ContentBlock array. Two independent reviews — Triage (T1.4) and JARVIS/FRIDAY (B1) — flagged that the flattened tail destroys the link between a sub-message's text and its attachments. The agent has no way to know which image belongs to which `<message>` tag.

**Decision:** rather than fix the wrapper-and-flatten scheme, the packing is restructured to **extend the existing per-arrival-event template and concatenate repetitions**. Each arrival event is emitted as `<sender_context>{json}</sender_context>\n\n{prompt}` (the format already produced by `adapter.rs:131-152`); attachments interleave immediately after their owning `<sender_context>` in arrival order. `<sender_context>` itself is the boundary marker — opening a new one in the stream marks the start of a new arrival event, and the previous one ends.

This subsumes both T1.4 and B1, removes the parallel sender-encoding scheme that T2.b (`from=` disambiguation) would have introduced, and folds T2.j's `arrived_at_relative` into a single additive `timestamp` field on the existing `<sender_context>` JSON.

---

## 2. Highest Guideline — Broker Structural Fidelity

Promoted to the top invariant of the entire batching design; the packing format below is one consequence of it:

> **The broker must faithfully preserve structural attribution: each chat-history arrival event (its sender, its text, its attachments) appears in the dispatched batch exactly as received — no merging, no splitting, no reordering, no attachment re-attribution, no heuristic pairing of related-looking messages, no semantic directives injected to instruct the agent how to interpret the input.**

The broker is a transparent buffer that extends the existing per-arrival-event template. `{prompt}` is placed verbatim — broker never parses, classifies, or transforms its content. Batched mode is just N repetitions of that template concatenated.

### 2.1 Concrete prohibitions derived from the invariant

The invariant above expands into five explicit "broker must not" rules. Together they form the test surface that any packing change is judged against:

1. **No cross-event reordering of ContentBlocks.** The dispatched `Vec<ContentBlock>` order must match arrival order across all event types (text, image, transcript, future block kinds). The broker may not sort, group-by-type, or hoist any block past an arrival-event boundary.
2. **No cross-event normalization.** The broker must not collapse two adjacent same-sender `<sender_context>` blocks into one; must not deduplicate identical `<sender_context>` records; must not factor out a "common header" across repetitions even when fields are byte-identical except for `timestamp`.
3. **No re-attribution of broker-injected metadata.** Any structural marker the broker adds (today: `<sender_context>` block) must remain identifiable as broker-emitted — it must never be mutated to look as if a participant produced it. If a future block is added by the broker (e.g. system notice), it must carry an unambiguous broker-origin marker, not appear inside a `<sender_context>` it did not own.
4. **No suppression of empty-prompt turns.** A buffered arrival event with `{prompt}` empty (e.g. attachment-only or voice-only message) must still be dispatched as its own `<sender_context>` repetition. The broker may not drop, merge into, or annotate-onto the previous event because "the prompt is empty."
5. **The judgment test.** Any proposed packing transformation is rejected if it makes the ACP agent unable to recover *which arrival event each attachment belonged to* using array adjacency alone (without consulting timestamps, sender IDs, or other heuristics). Adjacency-recoverability is the load-bearing property; everything else must yield to it.

**Scope clarification — inbound Discord field fidelity is broader than this ADR.** Today's broker (`discord.rs:480-483`) extracts only `msg.content` and `msg.attachments` from inbound Discord messages. Other fields — `embeds[]` (including auto-generated link previews), `stickers`, `reactions`, `reference` (reply chain) — are silently dropped. Dispatched ContentBlocks reflect only the fields openab currently ingests; the *Highest guideline* applies to those fields specifically. Closing the inbound-fidelity gap is tracked as a follow-up.

---

## 3. Packing Format

### 3.1 Per-arrival-event template (unchanged from `adapter.rs:131-152`)

```
<sender_context>
{json}
</sender_context>

{prompt}
[ContentBlock for attachment 1]
[ContentBlock for attachment 2]
…
```

`{json}` is the existing `SenderContext` record:

```json
{
  "schema": "openab.sender.v1",
  "sender_id": "…",
  "sender_name": "…",
  "display_name": "…",
  "channel": "discord|slack|gateway",
  "channel_id": "…",
  "is_bot": false,
  "timestamp": "2026-04-27T06:13:17.927Z"
}
```

### 3.2 Single additive schema change

`SenderContext` JSON gains a `timestamp` field — ISO 8601 UTC, **platform message creation time** (not broker dispatch time):

| Source | Value |
|---|---|
| Discord adapter | `msg.timestamp` (serenity 0.12 `Timestamp`, RFC 3339 by default) |
| Slack adapter | `slack_ts_to_iso8601(event.ts)` — converts epoch-seconds-with-fractional to ISO 8601 with millisecond precision |
| Gateway adapter | `chrono::Utc::now().to_rfc3339()` at receive time — best-effort for non-Discord/Slack channels; documented as approximate |

`schema` stays `openab.sender.v1` — the field is additive and existing parsers keep working. The field has two purposes:

1. **Distinguishability** — adjacent same-author repetitions become structurally distinct even when other JSON fields would otherwise be byte-identical.
2. **Subsumes T2.j** — the agent computes any relative offset (typing cadence, rapid-fire vs slow correction) directly from the absolute timestamps; no separate `arrived_at_relative` field needed.

### 3.3 Multi-message batch — concatenate repetitions

For `batch.len() == N` arrival events, the consumer emits the per-arrival-event template N times back-to-back. **No outer wrapper, no banner, no instruction string, no `<message index=N>` tags.** The next `<sender_context>` opening is itself the boundary marker.

**Example.** Two messages, each with text + one attachment:

- M1 = "look at this" + screenshot, sender = alice
- M2 = "listen to this" + audio transcript, sender = alice

```
Vec<ContentBlock>:
  Text  { "<sender_context>\n{...alice's JSON, timestamp=T1...}\n</sender_context>\n\nlook at this" }
  Image { screenshot }                  ← belongs to M1 (most recent <sender_context> preceding it)
  Text  { "<sender_context>\n{...alice's JSON, timestamp=T2...}\n</sender_context>\n\nlisten to this" }
  Text  { transcript content }          ← belongs to M2 (boundary moved when T2's <sender_context> opened)
```

What the agent reads when ContentBlocks are concatenated logically:

```
<sender_context>
{"schema":"openab.sender.v1","sender_id":"…","sender_name":"alice","display_name":"alice","channel":"discord","channel_id":"…","is_bot":false,"timestamp":"2026-04-26T18:33:19.912Z"}
</sender_context>

look at this
[ImageBlock — screenshot]

<sender_context>
{"schema":"openab.sender.v1","sender_id":"…","sender_name":"alice","display_name":"alice","channel":"discord","channel_id":"…","is_bot":false,"timestamp":"2026-04-26T18:33:23.105Z"}
</sender_context>

listen to this
[TextBlock — transcript content]
```

### 3.4 Properties

- **`{prompt}` handling unchanged.** Each repetition slots the user's content verbatim into the existing template. Broker does not parse, transform, or annotate `{prompt}`.
- **No new tags.** `<sender_context>` is the only structural marker. Every metadata field (sender, channel, timestamp) already lives in the JSON; adding wrapper attributes would re-encode information already present.
- **Attribution is structural via array position** — attachments belong to the most recent `<sender_context>` preceding them in the ContentBlock array. Mirrors Discord's per-message bubble (text + inline attachments rendered together).
- **Multiple attachments per message** group naturally — all of M1's images / transcripts sit between M1's `<sender_context>` and M2's `<sender_context>`, in arrival order.
- **No ACP protocol change.** Still just `Vec<ContentBlock>` with `Text` and other block types — grouping comes from `<sender_context>` block positions in the array.

### 3.5 Single uniform code path for `batch.len() == 1` and `batch.len() ≥ 2`

The packing is one template emitted N times — no special-case fast path for isolated messages. For `batch.len() == 1` the output is one `<sender_context>...</sender_context>\n\n{prompt}` repetition with attachments interleaved, structurally equivalent to today's per-message dispatch with two small differences:

1. `<sender_context>` JSON now carries a `timestamp` field (additive schema change).
2. `extra_blocks` (text transcripts, images) are placed **after** the message's `<sender_context>...{prompt}` block in arrival order, rather than today's asymmetric "text-prepended, image-appended" ordering at `adapter.rs:138-152`.

Concretely this means **STT voice-message transcripts move from before the `<sender_context>` block to after it** (Scenario D in §5). The boundary rule stays clean: `<sender_context>` always opens an arrival event, attachments always follow.

---

## 4. Three-Way Comparison

| Aspect | Current per-message (`adapter.rs:131-152`) | RFC MVP (Appendix A "Packing a batch") | This ADR |
|---|---|---|---|
| Sender attribution | `<sender_context>` JSON wrapper around prompt | New `<message index=N from="…">` attribute (parallel schema invented for the RFC) | **Reuse** existing `<sender_context>` JSON verbatim — adds `timestamp` field, nothing else |
| Per-batch wrapper | n/a (single message dispatched alone) | One combined `Text` block: banner + N `<message>` tags concatenated | One `Text` block per sub-message + interleaved attachment blocks; no outer wrapper |
| Banner / semantic framing | n/a | `[Batched: N messages…]` always emitted for batches | **None.** Broker injects no banner, no instruction, no metadata beyond what already lives in `<sender_context>` |
| Boundary marker | n/a (single message) | `<message index=N from="…">` opening + `</message>` close | The next `<sender_context>` opening *is* the boundary — no separate tag |
| Text extras (transcripts) | Prepended before main text (`adapter.rs:138-143`) | Flattened at end of ContentBlock array (after combined Text) | Interleaved between this message's `<sender_context>` and the next, in **arrival order** |
| Image extras | Appended after main text (`adapter.rs:148-152`) | Flattened at end of ContentBlock array | Same as text extras — interleaved in arrival order |
| Attachment ↔ message link | Implicit (single message — only one possible owner) | **Lost** — flattened blocks have no tie back to a sub-message (T1.4 / B1 blocker) | **Structural by adjacency** — attachments belong to the most recent preceding `<sender_context>` |
| `batch.len() == 1` vs `≥ 2` code paths | Baseline (only path) | Two paths (with/without banner-Text combination) | **Single uniform path** — N=1 is just one repetition of the same template |

---

## 5. Scope of Attribution — Structural vs Semantic

The packing preserves **structural** attribution: which attachment was uploaded as part of which arrival event (chat message). It deliberately does **not** attempt **semantic** attribution: which text refers to which attachment across separate arrival events. Cross-message linking is exactly the inference that should be left to the ACP agent, which has full conversation context.

Four scenarios the packing must handle correctly. (Sender-context JSON is abbreviated as `{alice, ts=T1}` etc. for readability — in the real ContentBlock stream it's the full JSON record.)

### Scenario A — text and image in the same chat message

(e.g. drag-and-drop with caption)

```
<sender_context>{alice, ts=T1}</sender_context>
look at this
[ImageBlock]
```

✅ The image follows alice's `<sender_context>` with no other `<sender_context>` between → belongs to alice's M1.

### Scenario B — text in one message, image in the next, same author

(very common human pattern: type the description, then paste / drop the image)

- M1 (alice): "see this image"
- M2 (alice): [image, no text]

```
<sender_context>{alice, ts=T1}</sender_context>
see this image

<sender_context>{alice, ts=T2}</sender_context>
[ImageBlock]
```

✅ Broker keeps the structural truth (image arrived as M2, alone). The agent reads identical `sender_id` on both `<sender_context>` blocks and trivially infers M1's "this image" refers to M2's attachment. The `timestamp` delta `T2 − T1` reinforces this when M1 and M2 are seconds apart, and disambiguates the two `<sender_context>` blocks even though their other fields are identical.

### Scenario C — fragmented multi-author batch

(alice's text → bob's interjection → alice's image)

- M1 (alice): "see this image"
- M2 (bob): "what?"
- M3 (alice): [image, no text]

```
<sender_context>{alice, sender_id=A, ts=T1}</sender_context>
see this image

<sender_context>{bob, sender_id=B, ts=T2}</sender_context>
what?

<sender_context>{alice, sender_id=A, ts=T3}</sender_context>
[ImageBlock]
```

✅ The broker does not try to "skip" bob's message or re-link alice's M1 ↔ M3 — that's a semantic decision. The repeated `sender_id=A` lets the agent group by stable user ID across non-adjacent messages; bob's interjection is preserved as-is so the agent can decide whether to address it (e.g. answer bob's "what?" while also processing alice's image).

### Scenario D — voice-only message in a batch (existing STT path)

- M1 (alice): "look at this" + screenshot
- M2 (alice): voice-only — `msg.content` empty; `discord.rs:524` produces a `[Voice message transcript]: …` Text block in `extra_blocks`
- M3 (bob): "what?"

```
<sender_context>{alice, ts=T1}</sender_context>
look at this
[ImageBlock]

<sender_context>{alice, ts=T2}</sender_context>

[Voice message transcript]: hey can we sync about the deploy

<sender_context>{bob, ts=T3}</sender_context>
what?
```

✅ M2's `{prompt}` is empty (line after `</sender_context>` is blank), and the transcript Text block lands immediately after as M2's first attachment.

**Behavior change vs. today:** in the current per-message path (`adapter.rs:138-143`) the transcript is *prepended* before `<sender_context>` so it reads as if it were the user's typed text. Under this ADR the transcript moves to *after* `<sender_context>`, owned by M2 like any other attachment. The boundary rule stays clean (`<sender_context>` always opens; attachments always follow) and the agent still sees the transcript content — just one block down.

**Rollback path if cross-agent smoke fails.** If a Phase 1 cross-agent smoke fixture (Scenario D against Claude Code, Cursor, and Copilot) shows any target regressing on voice-only handling, the response is a code change, not a runtime toggle: either revert the `pack_arrival_event` call for the single-message voice case, or land a hotfix PR re-introducing the `extra_blocks.len() == 1 && prompt.is_empty()` special case that treats the transcript as a `{prompt}` substitute (matching pre-ADR behavior; cross-link to §6.3, which already records this as the documented escape hatch). **No always-on feature flag.** A runtime flag would be permanent code-path surface area for what is intended as a one-shot transition; the cross-agent smoke fixture is the gate, and a hotfix PR is the rollback mechanism. Keeping a single uniform packer in steady state preserves the *Highest guideline*'s structural simplicity.

The principle (instance of the *Highest guideline*): **structural truth is non-negotiable, semantic interpretation is deferred.** Even if the broker *could* heuristically pair M1 and M3 (same author + image-less + image-only), doing so would either be wrong sometimes or conceal information the agent might want — and either way violates broker fidelity.

---

## 6. Alternatives Considered

### 6.1 RFC MVP wrapper-and-flatten — `<message index=N from="…">…</message>` plus tail-flattened `extra_blocks`

**Rejected.** Wraps each sub-message text in a parallel `<message>` schema (separate from `<sender_context>`), then concatenates all sub-messages' `extra_blocks` after the combined Text block. Two failures:

1. **Attribution loss (T1.4 / B1).** Image and transcript blocks at the tail have no tie back to a `<message index=N>` — agent can't know which image belongs to which sub-message.
2. **Parallel sender-encoding schemes.** The `from="alice"` attribute duplicates information already in `<sender_context>` JSON's `display_name`, and risks drift if one schema evolves and the other doesn't. T2.b's "use `sender_id` for disambiguation" then becomes a separate cleanup item.

### 6.2 RFC MVP wrapper, `extra_blocks` placed inside the `<message>` tag

A patch on 6.1: instead of flattening to the tail, place each sub-message's `extra_blocks` immediately after its `<message index=N>` tag (JARVIS's suggested fix). **Rejected** because the same fix is achievable using `<sender_context>` itself as the boundary marker — no need to introduce a parallel `<message>` schema. This ADR's design is the same fix expressed without the new wrapper tag.

### 6.3 Keep current asymmetric ordering (text-prepended, image-appended) as a special case

**Rejected.** Preserving the current `adapter.rs:138-152` ordering would require a special-case branch (`extra_blocks.len() == 1 && prompt.is_empty()` for the STT voice-only path) on every single-message dispatch. Single uniform code path beats a fast-path branch for a marginal Scenario D readability difference. Scenario D is a documented behavior change; if it proves disruptive in cross-agent smoke the special case is reversible.

### 6.4 Inject a leading `[Batched: N messages…]` banner string

**Rejected.** Violates the *Highest guideline* — broker injecting framing is a semantic directive ("treat these as one logical unit") that the agent can no longer un-see. Whether to treat the messages as one logical unit is precisely the kind of judgment the agent should make from the structural facts (same `sender_id`, close `timestamp` deltas, etc.), not from a broker hint.

### 6.5 Sidecar metadata block (JSON map)

A single trailing JSON block describing per-arrival attribution — e.g. `{"events":[{"index":0,"sender_id":"A","ts":"…","attachment_indices":[2,3]}, …]}` — appended once at the end of the ContentBlock array, with all `<sender_context>` headers removed and prompts concatenated.

**Rejected** for three independent reasons:

1. **Single-sequence readability.** ACP agents read the prompt as a top-to-bottom narrative; pushing attribution into a tail JSON forces the agent to cross-reference `attachment_indices` against array positions, which loses the affordance that adjacency provides for free. The whole point of repeating `<sender_context>` is that "what belongs to whom" is recoverable by linear reading.
2. **Parser coupling.** A sidecar JSON introduces a second schema (separate from `<sender_context>`) that every consumer must learn — duplicating the failure mode of §6.1's parallel `<message>` tag. Schema additions then have to be made in two places (sender_context JSON *and* the sidecar) or risk drift.
3. **ACP / tool-use mismatch risk.** Some ACP agents may treat trailing JSON as a tool-result fragment or post-prompt instruction. The semantics of "JSON block at end of `Vec<ContentBlock>`" is not part of ACP and would vary across Claude Code / Cursor / Copilot. `<sender_context>` is already an established convention that all current agents handle.

The repeating-envelope design achieves the same attribution recovery using one schema and one parsing rule.

---

## 7. Consequences

### Positive

- **Closes T1.4 and B1** with one structural change — attachment attribution is recoverable by adjacency.
- **No new schema invented.** Reuses `<sender_context>` (already known to every ACP agent that consumes today's per-message format) plus one additive `timestamp` field. `schema` stays `openab.sender.v1`.
- **Subsumes T2.b** (`sender_name` disambiguation) — `sender_id` is already in `<sender_context>` JSON.
- **Subsumes T2.j** (`arrived_at_relative` offset) — agent computes any relative offset from absolute `timestamp`s.
- **Single uniform code path.** N=1 and N≥2 share the exact same packer (`pack_arrival_event`).
- **No ACP protocol change.** Still `Vec<ContentBlock>` with existing block types.
- **Validated end-to-end on a staging deployment** (2026-04-27). Per-arrival shape and `timestamp` field confirmed under organic traffic; multi-message batch concatenation (batch_size = 2) confirmed to produce a single streaming-edit reply per batch.

### Negative

- **Scenario D regression in non-batched mode.** STT voice transcripts move from prepended-before-`<sender_context>` to appended-after, changing the read order for single-message voice dispatches. Reversible via a special case if cross-agent smoke shows real disruption.
- **`{prompt}` empty case is structurally valid.** Voice-only / attachment-only messages produce an empty line between `</sender_context>` and the first attachment block. Agents that strictly validate "non-empty prompt" need to relax that assumption — but this is already the case for any voice-only message under today's format.
- **Cross-agent recognition risk.** Multi-`<sender_context>` repetition is a new shape from the agent's perspective. Existing single-`<sender_context>` parsing should generalize naturally (it's just the same envelope opening twice), but Phase 1 should include a manual cross-agent smoke fixture against Claude Code, Cursor, and Copilot.
- **Token-cost surface widens.** Each repetition re-emits the full `<sender_context>` JSON. For multi-bot channels with `max_buffered_messages = 30`, the per-batch envelope overhead is non-trivial. RFC #580 T2.k's `max_batch_tokens` soft cap (Phase 1) bounds this — the drain stops when either `batch.len() == max_buffered_messages` or `cumulative_tokens + next.estimated_tokens > max_batch_tokens`, splitting only at message boundaries (per Compliance rule 7 in §8).

  **Observation spec (Phase 1 instrumentation).** Broker must emit three metrics so cost growth is measurable, not assumed:

  - `context_tokens_per_event` — token count of the rendered `<sender_context>` envelope alone (per arrival event), histogram per adapter.
  - `p95_batch_size` — p95 of `events_per_dispatch` over rolling 1h window, per `(adapter, channel)` pair.
  - `packed_block_count` — total ContentBlock count per dispatched batch (sender_context + prompt + attachment blocks), histogram.

  **Threshold for dedup re-evaluation:** when `p95_batch_size × context_tokens_per_event > 500 tokens` per dispatch on any production channel for a sustained 24h window, the broker team must re-open the dedup question (e.g. emit `<sender_context>` only when sender or timestamp delta changes). Below that threshold the envelope cost is below noise and the readability win of always-explicit headers wins. The threshold itself is a starting point and may be tuned based on Phase 1 data.

### Neutral

- **`<sender_context>` proliferation in agent-visible context.** The agent now sees N `<sender_context>` blocks per batched turn instead of one. This is the intended structural fact, not noise — agents that previously parsed exactly one block per turn need to handle the N≥2 case, but the parsing rule is the same.
- **`timestamp` is wall-clock visible.** Discord/Slack already display the same timestamps to all participants in the channel; this is not a new exposure.

---

## 8. Compliance

1. **Broker forwards `{prompt}` verbatim.** Broker must not parse, classify, transform, summarize, or annotate the user-supplied text content within `{prompt}`. Any future feature that needs to inspect `{prompt}` content must do so without mutating what the agent receives.

   **Counter-examples (prohibited):** broker stripping markdown formatting before dispatch; broker expanding Discord `<@123>` mentions to `@username` strings; broker appending an `[image attached]` string when an image accompanies the prompt; broker collapsing repeated whitespace; broker normalizing Unicode forms.

2. **No banners or framing strings.** Broker must not inject any leading or trailing instruction text into the dispatched batch (e.g. no `[Batched: N messages…]`, no `[End of batch]`). All metadata lives in `<sender_context>` JSON.
3. **No wrapper tags beyond `<sender_context>`.** Multi-message batches are produced by repeating the per-arrival template; no `<message>`, `<batch>`, or other wrapper schema is introduced. Future schema needs are extended as additive fields inside `<sender_context>` JSON, not as new XML tags.
4. **Attachment attribution is structural via array position.** Broker must place each arrival event's `extra_blocks` immediately after that event's `<sender_context>` in the ContentBlock array, in the same order they were received from the platform adapter. No reordering, no deduplication, no cross-arrival re-attribution.

   **Counter-examples (prohibited):** broker sorting `extra_blocks` by type (e.g. all images first, then transcripts); broker hoisting all images of a batch to a "gallery" section at the end; broker deduplicating two identical images sent in the same batch; broker re-attributing M2's image to M1 because M1 had text and M2 was image-only.

5. **`SenderContext` schema is additive.** New fields may be added under the `openab.sender.v1` name; field removal or semantic change requires a `v2` bump and a migration path for downstream agents.
6. **`timestamp` is platform message creation time when available.** Discord and Slack adapters must use the platform's own message creation timestamp. The gateway adapter's receive-time fallback must be documented as best-effort to downstream consumers.
7. **Splitting only at message boundaries.** When the token-budget cap (`max_batch_tokens`, RFC #580 T2.k) forces a batch to split across multiple ACP turns, the split must occur between two arrival events — never inside a single arrival event. A single oversized message dispatches alone; the broker does not truncate or summarize it.

### 8.1 Semantic neutrality — prohibited transformations

The following classes of transformation are categorically forbidden because they make semantic judgments the broker is not authorized to make. They are listed explicitly so future "small optimization" PRs can be rejected by reference rather than re-litigated:

- **No topic split.** Broker must not split a single arrival event into multiple ACP turns based on content (e.g. detecting "two questions in one message"). One arrival = one event in the dispatched batch.
- **No intent merge.** Broker must not coalesce two adjacent same-sender messages into a single event even when they appear to express one logical thought ("see this" + "[image]"). Each arrival keeps its own `<sender_context>`.
- **No sender collapse.** Broker must not merge multiple distinct `sender_id`s into a single header even when display names or roles match (e.g. two human users with the same name, or two bots with the same role). Each unique sender event gets its own `<sender_context>`.
- **No silent drop.** Broker must not omit an arrival event from a batch on the grounds that it appears redundant, off-topic, or empty. The agent decides what to do with it.
- **No ordering inversion.** Broker must not reorder events within a batch based on perceived priority, sender role, or content type. Arrival order from the platform adapter is preserved.

If a future feature genuinely requires one of these transformations, it belongs in the ACP agent (which has the semantic context to make the call), not in the broker. The broker's job ends at faithful structural transport.

---

## References

- RFC #580 — [Turn-boundary message batching](https://github.com/openabdev/openab/issues/580)
- [Community Triage Review (T1.4 batch packing attribution)](https://github.com/openabdev/openab/issues/580#issuecomment-4322581751) — surfaced the original flattened-tail attribution gap that this ADR closes
- [JARVIS + FRIDAY independent review (B1 `extra_blocks` flattened to tail)](https://github.com/openabdev/openab/issues/580#issuecomment-4324508396) — converged on the same gap independently
- [Packing — combined response to T1.4 + B1](https://github.com/openabdev/openab/issues/580#issuecomment-4325645814) — the reformed-packing proposal this ADR records
- ADR: [Multi-Platform Adapter Architecture](./multi-platform-adapters.md) — defines the `SenderContext` record this ADR extends
- ADR: [Custom Gateway for Webhook-Based Platform Integration](./custom-gateway.md) — establishes the ISO 8601 / RFC 3339 UTC timestamp convention this ADR extends to `<sender_context>` JSON; the two schemas (`openab.gateway.event.v1` and `openab.sender.v1`) remain independent.

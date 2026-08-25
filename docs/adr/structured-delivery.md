# ADR: Structured Delivery (multi-bubble replies)

- **Status:** Accepted (fork-local)
- **Date:** 2026-08-25
- **Scope:** fork-local. This ADR is **not** written for upstream contribution — it documents a deliberate divergence in this fork. Upstream compatibility is preserved only where it is free (see §6.3).
- **Related:** [`docs/output-directives.md`](../output-directives.md), [`docs/adr/turn-boundary-batching.md`](turn-boundary-batching.md) (per-thread serialization this design relies on), [`docs/adr/custom-gateway.md`](custom-gateway.md) (§4 ordering)
- **Anchor pinning:** all `file:line` references resolve at `8661f3f`. They will drift; the ADR documents the *design* relative to a stable base, not a moving target. New code (`structured_delivery.rs`) is described conceptually.

---

## 1. Context

### 1.1 Problem

A single ACP turn's `agent_message_chunk` events are concatenated into one `text_buf` (`adapter.rs:952`) and split only at the platform's length limit by `format::split_message` (`adapter.rs:1137`). That split is a **transport** concern — it carries no semantic information about where one thought ends and the next begins. A newline is explicitly *not* a bubble boundary: multi-line content (an address, a code block, copy-ready text) must stay in one message.

An agent designed around conversational beats has no way to express them. "on it" followed a moment later by "found it, your flight moved to 8pm" reads very differently from a single message containing both — and the second form is what OpenAB can currently produce.

### 1.2 Why at the broker layer

ACP has no concept of message boundaries within a turn. One `session/prompt` yields one response; `agent_message_chunk` is an append-only text stream. There are exactly three places the boundary could live:

1. **In the model's output, as an envelope** — the model plans N bubbles in one generation; the broker fans them out. No protocol change.
2. **In the protocol** — a non-standard `sessionUpdate` variant the broker sends immediately on arrival. Real per-bubble sequencing, but a protocol fork (see §5.2).
3. **In the agent, bypassing the broker** — the agent calls the platform REST API directly, as `docs/sendfiles.md` describes for attachments. Rejected in §5.3.

This ADR takes (1). It reuses the existing turn lifecycle unchanged and is reversible: a config flag returns the deployment to byte-identical current behavior.

### 1.3 Goals & non-goals

**Goals:** let one model generation produce an ordered list of chat bubbles; deliver them as separate platform messages in order; never expose the envelope to a user; leave the existing text path untouched by default.

**Non-goals:**

| Concern | Where it belongs |
|---|---|
| Re-inferring per bubble (true sequential generation) | Phase 4 — needs an ACP extension (§5.2) |
| Tool authorization / policy engine | The agent runtime (Phase 2), plus `openab-mcp`'s existing `tool_filter` / schema validation. The model may only *propose*; `next.type = tool` is recorded and otherwise ignored in Phase 1. |
| Long-term memory, recipes | The agent runtime, not the broker. `openab-agent` already carries the recipe mechanism (`skills.rs` / `SKILL.md`). (Event triage started here as a non-goal and moved into scope in Phase 3 — see §7.) |
| A `(turn_id, bubble_id)` delivery ledger | Not needed in Phase 1: bubbles are sent sequentially inside one turn, and `pool::with_connection` holds the per-thread mutex (`acp/pool.rs:737`) for the whole delivery, so two turns on one thread cannot interleave. There is no retry path to make idempotent. |
| Apple Messages / iMessage ingress | [`adr/imessage-integration.md`](imessage-integration.md) |
| Per-agent delivery config | `[agent]` is one agent per process (`config.rs:1667`); multi-agent means multiple Deployments (`docs/multi-agent.md`). A global `[delivery]` section is the correct granularity. |

---

## 2. Mechanism Decision

**Decision:** the agent emits a versioned JSON envelope (`openab.turn.v1`) as its turn output. When `[delivery] mode = "structured"`, the router parses it into an ordered `DeliveryPlan` and calls `ChatAdapter::send_message` once per bubble. When the mode is `text` (the default) nothing changes.

### 2.1 Three invariants

**I1 — Nothing reaches the user before the envelope validates.**

Structured mode must be known *before* the turn starts, not discovered at its end. `streaming` and `native` are decided at `adapter.rs:709` / `:720`, and the streaming placeholder is posted at `:777` — long before any output exists. A turn-final `[[delivery:...]]` directive therefore cannot prevent a half-written JSON object from being edited into a live message. Structured mode is a **construction-time** router setting; the directive is only an optional per-turn schema override.

The same reasoning forces `native` off, not just `streaming`: Slack's assistant mode opens a native stream and pushes deltas via `stream_append` (`adapter.rs:960`), which would stream raw JSON tokens. Structured mode forces `streaming = false`, `native = false`, and `keep_full_text = false` together. Typing / status indicators (`set_status`) are unaffected and stay on.

**I2 — The envelope is never visible.**

Raw JSON, a *truncated* envelope, and the directive header must never be delivered. This constrains the failure path, not just the happy path: a `fallback_text` policy cannot simply send the turn buffer, because that buffer is exactly where the JSON is. `strip_envelope` exists for this, and `StructuredError::found_envelope()` is the guard that decides whether it is required.

**I3 — The text path is unchanged.**

`mode = "text"` is the default (AGENTS.md rule 1). The structured branch is a `return` before `finalize_body`; every existing test over the text path must keep passing untouched.

### 2.2 Where the branch goes

The finalize sequence today (`adapter.rs:1108-1137`):

```
text_buf
→ split_delivery      (:1108)  directives + body
→ finalize_body       (:1114)  re-prepends the session-reset notice
→ display_for         (:1117)  prepends the tool summary ("✅ 2 tool(s)")
→ convert_tables      (:1136)
→ split_message       (:1137)
→ three write paths (native / placeholder+edit / send-once)
```

**The branch must sit between `:1108` and `:1114`.** Both of the next two steps prepend text to the body:

- `finalize_body` (`adapter.rs:195`) re-prepends `⚠️ _Session expired, starting fresh..._` when a tool advanced `answer_start` past it.
- `compose_display` (`adapter.rs:1525`) prepends the tool summary unless `tool_display = none`.

Either one placed before the parser would make every envelope fail to deserialize. Branching after `split_delivery` gets the body with directives already stripped and narration already trimmed (`keep_full_text = false` puts the envelope — emitted after the last tool — in the delivered slice).

The parser is nonetheless tolerant of leading text (`find_envelope_span` locates the envelope rather than assuming it spans the whole string), because the reset notice is seeded at the head of `text_buf` *before* any agent output (`adapter.rs:760`) and survives into the body when no tool ran. Tolerant parsing is a second line of defence, not a substitute for branching in the right place.

### 2.3 Envelope contract

```json
{
  "schema": "openab.turn.v1",
  "messages": [
    { "id": "bubble_1", "text": "bro perth is behind sydney" },
    { "id": "bubble_2", "text": "i'm not lying to make you feel smart" }
  ],
  "next": { "type": "stop" }
}
```

- Unknown fields are rejected (`deny_unknown_fields`) at both levels.
- `next.type` ∈ `stop` | `wait` | `silent` | `tool`. Omitted `next` means `stop` — a missing turn-level action should not cost the user their reply.
- `stop` and `wait` are identical in OpenAB (a turn always ends at the `session/prompt` response); the distinction is kept so the agent's intent reaches logs and Phase 4.
- `silent` sends nothing and **wins over** a non-empty `messages` (a contradictory turn resolves at parse time, logged, so the delivery loop only ever reads `bubbles`).
- `tool` is a *proposal*. The harness decides. Phase 1 records it and delivers the bubbles.

**Over-limit turns are rejected, not truncated.** More than `max_bubbles`, or a bubble over `max_bubble_chars`, fails the whole plan and falls back. Silently dropping a tail would read to the user as a complete answer — the one outcome worse than a plain-text reply.

---

## 3. Failure policy

`[delivery] on_parse_error` decides what the user sees. Every policy is envelope-safe:

| Policy | Behavior |
|---|---|
| `fallback_text` (default) | Deliver the turn body through the normal single-message path, with any envelope fragment removed by `strip_envelope`. Covers the common failure: the model forgot the envelope and answered in prose — the user still gets their reply. |
| `error_message` | Deliver a fixed line instead of the turn text. |
| `silent` | Deliver nothing. Logged only. |

`StructuredError::NotStructured` is the only variant where the raw body is safe to send verbatim, and it is exactly the "model answered in prose" case. Every other variant sets `found_envelope() == true` and routes through `strip_envelope` first. A truncated envelope is deliberately classified as *found* — being cut off mid-object is precisely when leaking is most likely.

`strip_envelope` only removes a fragment carrying `"schema"` or `"messages"` keys, so a prose answer that happens to contain an unrelated JSON snippet is returned unmangled.

---

## 4. Gateway ordering

Discord and Slack `send_message` await an HTTP round-trip, so sequential `await`s are ordered by construction. The Custom Gateway is not: `GatewayAdapter::send_gateway_reply` allocates a `request_id` and waits for the ack **only when `self.streaming` is true** (`gateway.rs:265`). Otherwise the reply is pushed onto the WebSocket and the call returns immediately — N bubbles become N in-flight messages whose processing order on the gateway side is not guaranteed.

**Decision:** structured mode requires ack-per-bubble on the gateway. `GatewayAdapter` gains an explicit `await_ack` flag rather than overloading `streaming`, whose name would then mean two unrelated things.

LINE needs no change: its reply token is consumed with `cache.remove` (`adapters/line.rs:678`), so bubble 1 uses the free Reply API and the rest fall through to Push. Correct, at the cost of push quota. Batching all bubbles into one Reply call (LINE accepts up to 5 messages) is a possible later optimization, not a correctness requirement.

---

## 5. Alternatives considered

### 5.1 A text marker (`[[bubble]]` on its own line)

Split the finalized text on a sentinel. ~50 lines, no JSON in the prompt, and it composes with the existing directive syntax.

Rejected as the primary mechanism: it cannot carry `next` (`silent` in particular — "decide not to reply" is a first-class outcome for a proactive agent), and a sentinel inside a fenced code block would split a bubble mid-content. Retained as a *possible* cheap A/B probe if the bubble experience itself needs validating before committing to the envelope — the two are not mutually exclusive, since `DeliveryMode` is an enum.

### 5.2 An ACP extension (`openab_message` sessionUpdate)

Emit each bubble as its own notification; the router sends on arrival. This is the only design that gives *true* sequential generation — the model decides the next bubble after the previous one has already landed, so it can acknowledge, run a tool, and then report what it found.

**Shipped in Phase 4** as `[delivery] mode = "sequential"`, alongside — not instead of — the envelope. See §8 for the extension's contract.

The envelope stays the recommended default: one model call per turn against one per bubble is a real price, and most replies do not need a beat that depends on work done mid-turn.

### 5.3 The agent calls the platform API directly

`docs/sendfiles.md` already documents this pattern for attachments, which OpenAB deliberately does not relay.

Rejected for text. It bypasses the trust gate (`adapter.rs:gate_incoming`), the per-thread serialization, `split_message`, `convert_tables`, and the reaction lifecycle — and it would require handing platform credentials to the agent, which AGENTS.md rule 3 exists to prevent.

---

## 6. Consequences

### 6.1 What changes

- New `crates/openab-core/src/structured_delivery.rs`: the wire schema, the pure parser, `DeliveryPlan`, and `strip_envelope`. Pure functions, no I/O.
- New `[delivery]` config section, defaulting to the current behavior.
- `AdapterRouter` gains a delivery setting via a `with_delivery` builder, mirroring `with_trust` (`adapter.rs:499`). One call site (`src/main.rs:859`).
- `OutputDirectives` gains `delivery` as an optional per-turn schema override.
- `GatewayAdapter` gains `await_ack` (§4).
- No `ChatAdapter` trait change. No ACP protocol change. Discord and Slack adapters unmodified.

### 6.2 What it costs

- Structured mode gives up token streaming. The user sees a typing indicator, then bubbles. For short conversational replies this is a better experience; for long-form answers it is worse — which is why the mode is global and opt-in rather than per-turn.
- The model must produce valid JSON every turn. The fallback path makes that non-fatal, but a model that drifts turns every reply into a single fallback message.

### 6.3 Upstream divergence

Structurally minimal. The schema identifier follows the repo's existing convention (`openab.sender.v1`, `openab.gateway.reply.v1`), the module's types are named for what they do rather than for any one product (`DeliveryMode`, `DeliveryPlan`, `StructuredError`), and the whole feature sits behind one config value. What diverges from upstream is the decision to carry the feature at all, not its shape.

---

## 7. Proactive events (Phase 3)

### 7.1 No second event protocol

The obvious move is a dedicated envelope for external events — a source, a type,
an opaque payload. It was rejected.

`openab.gateway.event.v1` already carries everything an inbound event needs, and
an arriving mail *is* "someone sent you something": the sender is the mail's
sender, the text is its summary, the channel is where the reply lands. Reusing it
inherits the trust gate, batching, session routing, STT, and attachments for free.
A parallel protocol would need every one of those re-implemented or explicitly
skipped, and "skipped" is how an unauthenticated path into the agent gets built.

What the existing event genuinely lacked is one bit: *did a human just send
this?* That is now an additive, defaulted `proactive` field. The schema string is
unchanged, and every existing source keeps working untouched.

### 7.2 Triage runs after trust, before dispatch

```
event → should_skip_event → trust gate → triage → dispatcher → agent
                             (L2/L3)     (§7.3)
```

**After the trust gate**, so an unauthorized source cannot burn a conversation's
daily allowance by spraying events at it. **Before dispatch**, so a suppressed
event costs no LLM call — which is the entire point.

Both gateway ingress paths (the WebSocket loop and the unified binary's webhook
bridge) call one shared `triage_gateway_event`, exactly as they already share
`gate_gateway_event`. A rule that applies on one path and not the other is worse
than no rule.

### 7.3 Two layers of quiet, and why both

| Layer | Cost | Decides |
|---|---|---|
| `[triage]` | free, deterministic | whether the agent is woken at all |
| The agent's `next: "silent"` | one LLM call | whether it has anything worth saying |

The second layer alone is sufficient and wrong: it spends a model call on every
routine notification that arrives at 3am, and it cannot dedupe a webhook retry
because each delivery looks like a fresh event to a stateless turn. The first
layer alone is also wrong — no rule can tell "your flight moved" from "your
receipt is ready". They compose: cheap rules decide whether the question is worth
asking, the model decides the answer.

### 7.4 What is deliberately not persisted

Triage counters are in-memory, like the other cross-turn caches in this crate. A
restart forgets the cooldown and the day's count.

That is the safe direction to be wrong in: a restart can allow one extra
proactive message, never silence a needed one. Durable counters, an
ignored-alert history, and per-source mutes belong to the agent runtime, which
owns the user-facing state anyway.

### 7.5 Suppression is logged, always

"The agent read the event and chose to stay quiet" and "the broker never asked
it" are indistinguishable from outside the process, and the difference is the
whole debugging story for a proactive agent that has gone silent. Every
suppression emits a structured line with a stable `reason` tag (`duplicate`,
`quiet_hours`, `cooldown`, `daily_cap`), kept separate from the human-readable
message so rewording it cannot break a dashboard.

---

## 8. The sequential extension (Phase 4)

### 8.1 Contract

A non-standard `session/update` variant, deliberately sharing `agent_message_chunk`'s `content: {type, text}` shape so agents and readers reuse one convention:

```json
{
  "sessionUpdate": "openab_message",
  "id": "bubble_1",
  "content": { "type": "text", "text": "on it" }
}
```

`id` is for correlation in logs only. A malformed event — no text — classifies as nothing at all rather than as an empty message, so a broken agent cannot deliver blank bubbles.

### 8.2 Degradation is the compatibility story

Agents that do not implement the extension never emit the event. In `sequential` mode a turn that emits no bubbles falls through to the plain-text path, so the user still gets the reply. There is no capability negotiation and no version handshake: the absence of an event *is* the negotiation.

The reverse — an agent emitting the event at a broker not in `sequential` mode — is logged loudly and ignored, because silently dropping something the agent believes it said is the worse failure.

### 8.3 The agent's `reply` tool changes shape, not just behavior

In envelope mode `reply` is terminal and carries `next` (`stop` / `wait` / `silent`). In sequential mode it is not terminal, and `next` is **removed from the schema**: the turn ends when the model stops calling tools, and staying silent means never calling `reply`. Leaving `next` in place would let the model declare an intention the loop does not honour — a contract the code does not keep is worse than no contract.

The tool the model is shown and the way the loop treats it are derived from one flag, so they cannot drift apart.

### 8.4 Failure after the first bubble

The question §5.2 deferred. The answer is: **the user keeps what already arrived**, and the turn is marked failed.

Both sides stop at the first failure rather than pressing on — a reply with a hole in the middle is worse than a short one. The agent returns an error when the host channel is gone (finishing a reply nobody will receive is pointless); the broker abandons the remaining bubbles and returns `Err`, so dispatch surfaces ❌ rather than 🆗 over a partial turn.

### 8.5 Cancellation

Unchanged, and adequate: `pool::with_connection` holds the per-thread mutex for the whole turn, so a user's new message queues rather than interleaving with bubbles still being emitted. Existing `session/cancel` handling ends the turn; bubbles already delivered stay delivered, which is the only honest outcome for messages the user has already read.

---

## 9. Verifying it

`scripts/bubble-test/` runs every delivery path in this ADR offline — a fake
gateway that prints each delivered message with the gap since the last one, and a
fake ACP agent that replays scripted turns. No LLM key, no platform account, no
network. See its [README](../../scripts/bubble-test/README.md).

What that covers is the broker: bubble boundaries, ordering, inter-bubble timing,
envelope leak-proofing, triage decisions, and the sequential extension. What it
cannot cover is whether a *real model* produces good bubbles — that needs a live
agent, and it is a judgement call rather than an assertion.

---

## 10. Phases

| Phase | Contents |
|---|---|
| **0** | `structured_delivery.rs` + unit tests. Not wired; no behavior change. |
| **1** | `[delivery]` config, router branch, per-bubble send loop, gateway `await_ack`, docs. |
| **2** | ✅ Agent-side: `openab-agent`'s `turn_envelope` module constrains replies through a `reply` tool whose input schema *is* the envelope, so the provider API guarantees well-formed JSON. Voice reuses the existing `AGENTS.md` mechanism; recipes reuse `skills.rs`. Memory and a policy engine follow once the bubble experience is validated. |
| **3** | ✅ Proactive events. Ingress reuses `openab.gateway.event.v1` with an additive `proactive` flag — no second event protocol. `event_triage` adds dedupe / quiet hours / cooldown / daily cap ahead of dispatch, shared by both gateway ingress paths, and suppressions are logged with a stable reason tag. |
| **4** | ✅ True sequential bubbles: `AcpEvent::Message` + immediate delivery in the broker, a non-terminal `reply` tool in `openab-agent`, both behind `mode = "sequential"`. The envelope stays the default (§8). |

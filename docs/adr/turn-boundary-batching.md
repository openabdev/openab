# ADR: Turn-Boundary Message Batching

- **Status:** Proposed
- **Date:** 2026-04-29
- **Author:** @brettchien
- **Tracking issue:** [#580](https://github.com/openabdev/openab/issues/580) — kept as historical discussion record
- **Implementation PR:** [#686](https://github.com/openabdev/openab/pull/686) (Phase 1 wiring; this ADR documents the design it lands)
- **Related:** [#78](https://github.com/openabdev/openab/issues/78) (Session Management — precondition), [#58](https://github.com/openabdev/openab/issues/58) (per-connection locking — precondition), [#307](https://github.com/openabdev/openab/issues/307) (cross-session blocking — adjacent symptom of §2.7)
- **Anchor pinning:** All file:line references in this document are anchored to **v0.8.2-beta.1** ([`52052b8`](https://github.com/openabdev/openab/commit/52052b8b104a85a7073dd6ae99eeb9f2fd331abe)). New modules introduced by the Phase 1 PR (e.g. `src/dispatch.rs`) are described conceptually, without line-number anchors, since they do not exist in the released base.

---

## 1. Context

### 1.1 Problem

Within one thread, openab today processes one user message per ACP turn. After RFC #78 §2b each thread has its own `Arc<Mutex<AcpConnection>>` (`pool.rs:15`), so inter-thread isolation is solved — but **inside one thread**, messages that arrive while a turn is running become independent `tokio::spawn` tasks racing for the per-connection mutex (`discord.rs:600-608`), each ending up dispatched as a separate sequential ACP turn.

`adapter.rs:181` → `pool.with_connection(thread_key, |conn| { ... })` (`pool.rs:223`) calls `conn.session_prompt(content_blocks).await` (`adapter.rs:240`) exactly once per call. `content_blocks` is built from one user message — its prompt text plus that message's own image / transcript blocks (`adapter.rs:131-152`). Multiple `ContentBlock`s in one turn means "one message with multiple media parts," never "multiple messages."

Tokio's `Mutex` keeps a fair-ish FIFO `LinkedList<Waker>` but [does not guarantee strict FIFO](https://docs.rs/tokio/latest/tokio/sync/struct.Mutex.html); the mutex only sees "someone is waiting" — wakers are opaque, so it cannot enumerate pending messages, inspect content, or batch them. Batching therefore can't be retrofitted onto the mutex; it requires an explicit queue at a layer that owns the message data.

Three workload patterns this hurts:

1. **Stream-of-thought split** — `"can you check the build"` → `"actually wait"` → `"check the build *and* run the e2e tests"` in 5 seconds. Today: 3 sequential turns; turn 1 wastes work, turn 2 reacts before seeing the correction, turn 3 finally has the full intent.
2. **Late attachment / clarification** — text question, then 8 seconds later the screenshot. Today: 2 turns, the first answers without the screenshot.
3. **Independent topics interleaved** — two unrelated asks back-to-back. The broker should merge them into one ACP turn; the agent answers both in one response (agents handle multi-intent prompts well).

### 1.2 Why at the broker layer

ACP coding CLIs — Claude Code, Cursor, Codex — consume one turn at a time: each `session_prompt` is one input → one response. They do not inspect incoming chat traffic and do not batch messages themselves. Adapter-level pre-turn debouncing (e.g. Hermes' `_pending_text_batches`) imposes a latency floor on every message including isolated ones, which conflicts with the zero-latency-first-message goal. The broker is the only layer that can buffer *during* an in-flight turn (when the user is already waiting on the agent) and dispatch at turn completion, paying zero added latency for the first message and amortizing nothing for subsequent ones.

Per-thread scope is required because conversation scope in openab = thread; per-thread keying matches the existing `Arc<Mutex<AcpConnection>>` keying (`adapter.rs:154-161`). Per-channel or global merging would conflate independent conversations.

### 1.3 Goals & non-goals

**Goals:** replace **1 message → 1 turn** with **N messages-arrived-during-turn → 1 next turn** within a single thread; introduce the data structure required (a per-thread bounded `mpsc::channel`); achieve deterministic same-thread ordering as a side benefit.

**Non-goals:**

| Concern | Layer that owns it |
|---|---|
| Inter-thread isolation | Per-connection mutex (RFC #78 §2b / #58). Precondition. |
| Cross-session blocking (#307) | Different layer — about a *new* thread's session unable to start. |
| Pre-turn debouncing | Rejected; see §5.1. |
| Topic detection / semantic grouping | Deferred to ACP agent (it has the context and inference budget). |
| Cancelling / restarting in-flight turns | Existing `/cancel` semantics unchanged. `/cancel-all` covers buffered-message drop (Phase 1, §4.4). |
| Persisting buffer across pod restarts | Buffer only exists during in-flight turn — restart loses the in-flight turn anyway, so buffered messages share its fate. |
| Replacing the per-connection mutex | Mutex stays exactly as RFC #78 §2b describes it. |

---

## 2. Mechanism Decision

**Decision:** introduce a per-thread bounded `mpsc::channel` plus a long-lived consumer task. The producer (platform event loop) sends each arrival into the channel via `Dispatcher::submit`. The consumer drains greedily at turn boundaries and dispatches the batch as **one** ACP `session_prompt` call. The packing of those N arrival events into `Vec<ContentBlock>` is specified in §3.

### 2.1 Three invariants

The design rests on three structural invariants. All later choices in §2 and §3 are concrete instances of one of these.

**I1 — First message after idle has zero added latency.** The first arrival on an idle thread fires immediately. The buffer only fills *during an active turn*, when the user is already waiting on the agent. The agent's turn duration is itself a natural "user is waiting" window, used for free.

**I2 — At most one in-flight turn per thread.** All buffering happens between turns, never within. The per-connection mutex plus the per-thread consumer task together enforce this — the consumer drains, fires `session_prompt`, awaits completion, then loops back to `recv`. No two `session_prompt` calls overlap on the same thread.

**I3 — Broker structural fidelity (top-level invariant).**

> The broker must faithfully preserve structural attribution: each chat-history arrival event (its sender, its text, its attachments) appears in the dispatched batch exactly as received — no merging, no splitting, no reordering, no attachment re-attribution, no heuristic pairing of related-looking messages, no semantic directives injected to instruct the agent how to interpret the input.

The broker is a transparent buffer that extends the existing per-arrival-event template (`<sender_context>...</sender_context>\n\n{prompt}`, `adapter.rs:131-152`). `{prompt}` is placed verbatim — broker never parses or transforms its content. Batched mode is just N repetitions of that template concatenated.

I3 is load-bearing for §3 (packing) and §6.4 (compliance rules). Every transformation the broker would apply is information the ACP agent can no longer recover; the rule is "must not", not "should not".

### 2.2 Architecture

```
state                    event                              action
────────────────────────────────────────────────────────────────────────
thread idle              M1 arrives                          fire turn 1 with M1 immediately
turn 1 in flight         M2 arrives                          send M2 into channel
turn 1 in flight         M3 arrives                          send M3 into channel
turn 1 completes         (consumer recv wakes)               drain channel → fire turn 2 with [M2, M3]
turn 2 in flight         M4 arrives                          send M4 into channel
turn 2 completes         (consumer recv wakes)               drain channel → fire turn 3 with [M4]
turn 3 completes         (channel empty)                     consumer parks on recv → awaits next message
```

```
┌─────────────────────────────────────────────────────────────────┐
│ Platform event loop (Discord / Slack / Gateway adapter)         │
│  inbound msg ──gates (allow_*, bot_turns, mentions)──▶           │
└────────────────────────────────────┬─────────────────────────────┘
                                     │  (per-message tokio::spawn)
                                     ▼
                          ┌────────────────────────┐
                          │   Dispatcher           │
                          │   per_thread:          │
                          │   HashMap<key, Handle> │   ◀── std::sync::Mutex (§2.5)
                          │   (lazy insert)        │
                          └─────────┬──────────────┘
                                    │  tx.send(BufferedMessage).await
                                    ▼
   ┌──────────────────────────────────────────────────────────────┐
   │ ThreadHandle (one per active thread):                        │
   │   tx: mpsc::Sender<BufferedMessage>      (cap = max_buffered)│
   │   _consumer: JoinHandle (consumer_loop task)                 │
   │   generation: u64                                            │
   └──────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
   ┌──────────────────────────────────────────────────────────────┐
   │ consumer_loop (one per active thread):                       │
   │   loop {                                                     │
   │     first = rx.recv().await       // I1: 1st msg has 0 latency│
   │     batch = greedy_drain(first, max_buffered, max_tokens)    │
   │     dispatch_batch(...)           // pack (§3) + session_prompt│
   │   }                                                          │
   └────────────────────┬─────────────────────────────────────────┘
                        │  pool.with_connection(thread_key, ...)
                        ▼
   ┌──────────────────────────────────────────────────────────────┐
   │ SessionPool / AcpConnection (unchanged from v0.8.2-beta.1)   │
   │   conn.session_prompt(Vec<ContentBlock>).await               │
   └──────────────────────────────────────────────────────────────┘
```

The `Dispatcher` sits **above** `SessionPool` in the call graph. Per-thread keying matches the existing `thread_id` keying from `pool.rs:15`. The per-connection mutex still wraps each ACP turn but no longer determines message order — ordering moves to the per-thread mpsc enqueue (μs-scale handle lookup) instead of the per-connection mutex's waker list (held during the 30s+ ACP turn).

**Wiring (Phase 1, Discord + Slack + Gateway — see §4.4):**

```rust
// At v0.8.2-beta.1 (discord.rs:600-608):
tokio::spawn(async move { router.handle_message(...).await });

// After Phase 1 — all three modes go through the same dispatcher; the mode
// only changes (cap, grouping, idle_timeout). The dispatcher's `key()`
// computes the mpsc identity from grouping; `Per-Message` reduces to a
// 1-deep buffer per thread (each message dispatches alone, FIFO).
tokio::spawn(async move {
    dispatcher.submit(
        thread_key, thread_channel, adapter, buf, other_bot_present,
    ).await
});

// where (cap, grouping, idle) are derived from message_processing_mode:
//   PerMessage -> (1,                   Thread, PER_MESSAGE_CONSUMER_IDLE_TIMEOUT)
//   PerThread  -> (max_buffered_messages, Thread, DEFAULT_CONSUMER_IDLE_TIMEOUT)
//   PerLane    -> (max_buffered_messages, Lane,   DEFAULT_CONSUMER_IDLE_TIMEOUT)
```

`PerMessage` is structurally a uniform-path special case (cap=1; consumer drains exactly one message per turn). `PerThread` and `PerLane` differ only in dispatcher key shape — `PerThread` keys mpsc identity by `(platform, thread_id)`, `PerLane` keys by `(platform, thread_id, sender_id)` so each sender owns a separate buffer + consumer (still serialized through the shared `Arc<Mutex<AcpConnection>>` per thread). See §4.1 for config-side rationale.

### 2.3 Producer / consumer lifecycle

Each active thread owns a bounded `mpsc::channel` (capacity = `max_buffered_messages` from config) and a long-lived consumer task that drains it. The struct shape:

```rust
struct BufferedMessage {
    prompt: String,
    extra_blocks: Vec<ContentBlock>,
    sender_json: String,            // serialized SenderContext (see §3.2)
    trigger_msg: MessageRef,        // anchor for reactions
    arrived_at: Instant,
    sender_name: String,            // display name (not a stable user ID)
    estimated_tokens: usize,        // for max_batch_tokens cap (§4.3)
}

struct ThreadHandle {
    tx: mpsc::Sender<BufferedMessage>,
    _consumer: tokio::task::JoinHandle<()>,
    generation: u64,                // race-safe eviction (§2.5)
    channel_id: String,             // for shutdown logging (§6.6)
    adapter_kind: String,           // "discord" / "slack" / "gateway"
}

pub struct Dispatcher {
    per_thread: std::sync::Mutex<HashMap<String, ThreadHandle>>,
    router: Arc<Router>,
    max_buffered_messages: usize,
    max_batch_tokens: usize,
    // other_bot_present is passed per-call to submit() and captured by each consumer task (§2.6)
}
```

**`submit(thread_key, thread_channel, adapter, msg, other_bot_present)`** (called from the platform event loop's per-message `tokio::spawn`'d task):

1. Lock `per_thread`; lazily construct the `ThreadHandle` if absent — creates `mpsc::channel(max_buffered_messages)`, spawns the consumer task with the relevant `Arc<AtomicBool>` for `other_bot_present`, initialises `generation = 0`; release the lock. On SendError eviction (§2.5), the replacement handle gets `generation = old + 1`.
2. `tx.send(msg).await` — returns immediately if the channel has space; **parks the calling task if the channel is full**. Only this `submit` future blocks; the platform event loop is unaffected because `submit` runs inside its own `tokio::spawn`'d task per inbound message.
3. On `SendError` (consumer task has died): see §2.5.

**`consumer_loop(thread_key)`** (one per active thread, lives until the channel closes):

1. `let first = rx.recv().await` — blocks until at least one message arrives. (I1: zero latency on first message after idle.)
2. **Greedy drain** with two stop conditions:
   - `batch.len() == max_buffered_messages`, or
   - `cumulative_tokens + next.estimated_tokens > max_batch_tokens` (soft cap, §4.3).
3. **Read freshness inputs at dispatch time** — re-load `other_bot_present` from the per-thread `Arc<AtomicBool>` mirror (§2.6); the `BufferedMessage`-attached snapshot is not used.
4. Dispatch as **one ACP turn** via `dispatch_batch` — which applies 👀 reactions to all batch messages (§6.7), packs via `pack_arrival_event` (§3), then calls `pool.with_connection` + `session_prompt`.
5. Loop back to step 1.

**Idle eviction:** when `cleanup_idle` (`pool.rs:295`) evicts a thread, the dispatcher drops its `ThreadHandle` → `Sender` drops → channel closes → `recv()` returns `None` → consumer exits cleanly. No leader-election race; there is always exactly one consumer per active thread.

### 2.4 Producer-side gating (multi-party)

**Buffer invariant: by the time a message reaches the buffer, it has already been determined to be intended for our agent.** All multi-party complexity lives upstream of `submit`:

| Gate | Source | Multi-party role |
|---|---|---|
| `allow_bot_messages` (off / mentions / all) | `slack.rs:710-765` | Whether bot messages enter at all. |
| `allow_user_messages` (involved / mentions / multibot-mentions) | `slack.rs:768-810` | Which human messages we respond to. |
| `trusted_bot_ids` | config | Whitelist for `mentions` / `all` modes. |
| `bot_turns` consecutive-bot limit | `slack.rs:672-696` | Loop guard. **Per-message at ingest, not per batch.** |
| Eager multi-bot detection | `slack.rs:649-657` | Sets `other_bot_present` → triggers `multibot-mentions` semantics. |

Implications for the dispatcher:

- `other_bot_present` is a per-thread fact set upstream; the dispatcher mirrors it into an `Arc<AtomicBool>` and reads it at dispatch time (§2.6).
- `MAX_CONSECUTIVE_BOT_TURNS` runs *before* `submit`; batching is downstream and cannot bypass it. Bot-turn-limit counts batches as turns (one ACP invocation = one logical turn); the per-message ingest counter is unchanged.
- Per-sub-message attribution in a batch is carried by repeated `<sender_context>` headers (§3).

### 2.5 Error handling on consumer death

**`SendError` is a real surface introduced by the per-thread-consumer architecture** — `tx.send().await` returns `Err` only when the receiver half is dropped, which happens when the consumer task exits unexpectedly (panic inside `dispatch_batch` or its callees; process tear-down). v0.8.2-beta.1's per-message direct-dispatch has no analogous failure mode.

**Decision: early-error, no auto-retry.** The handler:

1. Evict the stale `ThreadHandle` from `per_thread` — next `submit` constructs a fresh consumer lazily.
2. `reactions.set_error()` on `msg.trigger_msg` — ❌ anchored to the failing arrival.
3. `adapter.send_message(thread_channel, format!("⚠️ {}", format_user_error(...)))` — actionable description, reuses the existing helper from `pool.get_or_create` Err handling (`adapter.rs:182-189`); no new user-facing strings.
4. `return Err(e)` to the caller.

**Why both signals.** ❌ on `msg.trigger_msg` answers *"which message failed?"* (anchored to a specific message ID, survives scrolling). The `⚠️` text answers *"why did it fail / what should I do?"*. In per-message dispatch they were partly redundant; in batched dispatch with rapid-fire M1/M2/M3 each carries distinct load. The shape mirrors `stream_prompt`'s Err handler in `handle_message` (`adapter.rs:212-234`: ❌ + ⚠️ + return `Err`).

**Why no auto-retry — three reasons converge:**

- **Consistency.** Released code's contract is "if dispatch fails, you see `⚠️` and re-send if you still want." The dispatcher inherits that contract instead of inventing a parallel "broker silently retries N times" path.
- **`SendError` is rare and informative.** Only consumer-death conditions surface it. That's a real bug — surface and log is the right response, not papering over with retries.
- **No spin-loop possible.** With no retry path, the "`SendError → evict → retry` on permanently broken stdio" scenario cannot materialize. Retry-budget mechanics dissolve.

**Race-safe eviction.** Two `submit` calls can observe `SendError` on the same handle concurrently. Eviction happens under the `per_thread` lock with the `generation: u64` counter on `ThreadHandle` — only the first observer evicts. The second observer takes the lock and either (a) finds the entry gone, or (b) finds the entry already replaced by a third concurrent `submit` (newer generation than its own captured `tx`); in both cases its `tx` is the stale one, so it surfaces the error without re-evicting. Reconstruction is always lazy — the *next* `submit` after eviction creates a fresh handle through the normal map-insert path. Each observer reacts on its *own* arrival message (different `msg.trigger_msg` IDs) and produces its *own* `⚠️` text — concurrent failures yield concurrent (reaction + text) pairs on the right targets, no cross-attribution.

**`per_thread` uses `std::sync::Mutex`, not `tokio::Mutex`.** The critical section is a synchronous `HashMap` get/insert/remove with no `.await` inside; the async-machinery cost of `tokio::Mutex` buys nothing. `generation: u64` is plain (not atomic) because every read and write happens inside the `per_thread` lock — the surrounding mutex provides ordering.

**Disjoint from `/cancel-all`** (§4.4). `cancel_buffered` removes the handle from `per_thread` *before* aborting the consumer, so any *fresh* `submit` arriving after lands on the lazily-constructed new handle — no `SendError` on that path. Producers already parked in `tx.send().await` wake with `Err` and re-enter SendError recovery, but find the entry already gone and take the (a) branch above.

**`/cancel-all` race with concurrent `submit` is intentional.** If a `submit` arrives in the window between `cancel_buffered` removing the old handle and the next `submit` constructing a new one, that new `submit` creates a fresh consumer via the normal lazy-insert path. This is by design: `cancel_buffered` clears only the messages buffered at cancel time; messages that arrive after the cancel are treated as a fresh conversation start.

**Residual losses (same shape as a pod restart mid-turn):**

- **In-flight batch in the dead consumer's frame.** Lost when the panic unwinds. These messages can't be reacted from the SendError path because their `submit` already returned `Ok` before the consumer died.
- **Already-enqueued mpsc messages** (in the queue but not yet drained). Lost when `Receiver` drops.

A future supervisor catching consumer-task panic could iterate the in-flight batch and react ❌ on each; out of Phase 1 scope.

### 2.6 `other_bot_present` freshness

**Required invariant:** the dispatcher must read dispatch-time state, not consumer-spawn-time snapshot. If a new bot joins the thread mid-conversation, a stale snapshot misclassifies the batch's addressee semantics.

**Mechanism.** Mirror the producer-side `multibot_threads` cache into a Dispatcher-owned per-thread `Arc<AtomicBool>` — written by the producer's early-detect path (`slack.rs:649-657`, Discord analog), read by the consumer immediately before each `dispatch_batch`. `BufferedMessage` does not carry a per-message `other_bot_present` snapshot.

**Why mirror, not dereference adapter state.** Co-locating the `Arc<AtomicBool>` with the producer-side detection and letting the consumer dereference into adapter state would reverse the dependency: the dispatcher would have to know about platform-adapter internals. The mirror keeps the freshness invariant inside the dispatcher's contract.

### 2.7 `last_active` semantics — deferred

`submit` does **not** touch `last_active`. The single `last_active: Instant` lives on `AcpConnection` (`connection.rs:120`) and is touched at the start of `session_prompt()` (`connection.rs:430`) and again on `prompt_done()` (`connection.rs:468`); both run inside `stream_prompt`'s (`adapter.rs:238`) `pool.with_connection` lock guard. Batched dispatch preserves this exactly — the per-batch `session_prompt` call is the only liveness signal, just as in v0.8.2-beta.1.

**Pre-existing concern (not closed by this ADR).** The actual zombie mechanism in v0.8.2-beta.1 is not `last_active` staleness but `cleanup_idle`'s `try_lock` on the connection (`pool.rs:312-313`, "A busy session is not idle"): the lock attempt sees the in-flight task holding the mutex while `await`-ing a hung ACP and skips the candidate before the predicate is even evaluated. The slot stays occupied until the ACP process is killed externally.

**Two axes of impact:**

- **Axis 1 — zombie's own lifetime (same in both modes).** The connection mutex is held by `stream_prompt.await` in either model; `cleanup_idle.try_lock` skips it identically; the slot stays occupied until the ACP process is killed externally or the holder task finally exits. Dispatch mode does not change this.
- **Axis 2 — user-visible blast radius (worse under batching).** In per-message dispatch the second user message after a hang immediately blocks at the per-thread connection lock — the user sees "no reply" within seconds and stops sending. In batched dispatch, `submit` returns `Ok` instantly while messages accumulate in the per-thread mpsc buffer; the user keeps typing. When the consumer eventually dies (panic) or the `AcpConnection` is force-evicted, up to `max_buffered_messages` user messages can disappear at once, versus ≤1 in per-message dispatch.

**Existing related work — none of which closes this concern:**

- **#309 / PR #310** (closed 2026-04-13) — process-group kill + session suspend/resume. Fixes orphaned grandchildren after eviction but eviction still keys on the same single `last_active`.
- **#58** (closed) — pool write-lock held during entire `stream_prompt`. Fixed via lock-granularity refactor; `last_active` semantics untouched.
- **#78** (open RFC) — Session Management; §1b proposes `idle_timeout_minutes` vs `session_ttl_hours` split (duration-axis layering, not the indicator split needed here).
- **#307** (open) — adjacent symptom of the same `try_lock` blocker; would partially benefit from a fix.

The fix (indicator split + `cleanup_idle.try_lock` rework) is tracked in a dedicated follow-up issue, cross-referenced with #307. It is out of scope for this ADR because it touches `pool.rs` eviction semantics independently of batching.

### 2.8 Benefits of N→1

Falling out of N-messages-into-1-turn (not the primary motivation, but real):

**Token cost.** Each ACP turn re-sends `system + tools + accumulated history + new input`. Three sequential turns:

```
T1 input  = sys + tools + M1
T1 output = R1                       ← may be wasted
T2 input  = sys + tools + M1 + R1 + M2
T2 output = R2
T3 input  = sys + tools + M1 + R1 + M2 + R2 + M3
T3 output = R3
```

vs one batched turn: `input = sys + tools + [M1, M2, M3]`, output = single response. Saved tokens come from (in descending impact): wasted intermediate output, redundant tool invocations, intermediate responses re-fed, prompt-prefix cache invalidations.

**Latency.** Three sequential turns ≈ `T1 + T2 + T3` wall-clock; the batched path ≈ `T1 + T_batch` (M1 fires alone immediately; M2 and M3 merge into one follow-up turn). Leading-message latency is unchanged (I1).

**Deterministic ordering.** Same-thread ordering moves from approximate (`tokio::spawn` race + Tokio Mutex's not-strictly-FIFO waiter list, sync point held during 30s+ ACP turn) to strict (mpsc FIFO, sync point μs-scale on dispatcher mutex).

These benefits scale with input fragmentation and do not apply to isolated messages (buffer never fills).

---

## 3. Packing Format Decision

**Decision:** the broker emits **N repetitions of the per-arrival-event template** — a standalone `<sender_context>{json}</sender_context>` Text block, followed by transcript Text blocks (if any), followed by `{prompt}` as its own Text block (omitted if empty), followed by non-Text attachments — concatenated into one `Vec<ContentBlock>`. `<sender_context>` is its own ContentBlock and serves as a structural delimiter; the next opening of `<sender_context>` ends the previous arrival event. One additive schema bump (`SenderContext.timestamp`, ISO 8601 UTC) makes adjacent same-author repetitions distinguishable.

This subsumes T1.4 / B1 (attribution of attachments to their owning sub-message), T2.b (`sender_name` disambiguation — handled by the existing `sender_id` field), and T2.j (`arrived_at_relative` — agent computes from absolute timestamps).

The chosen design preserves the existing per-arrival template from `adapter.rs:131-152`, so single-message dispatch is byte-identical to v0.8.2-beta.1 except for the additive `timestamp` field and one ordering change for STT voice transcripts (§3.6 Scenario D).

### 3.1 Per-arrival-event template

Per arrival event, `pack_arrival_event` emits this sequence of `ContentBlock`s:

```
ContentBlock::Text { "<sender_context>\n{json}\n</sender_context>" }   ← standalone delimiter
[ContentBlock::Text from extra_blocks — e.g. STT transcripts, in arrival order]
ContentBlock::Text { "{prompt}" }                                       ← omitted if {prompt} is empty
[non-Text ContentBlocks from extra_blocks — e.g. ImageBlock, in arrival order]
```

`<sender_context>` is its own block so that, in batched dispatch, agents can scan the `Vec<ContentBlock>` for `<sender_context>` openers to find arrival boundaries without parsing inside any single Text block. Within an arrival, transcripts precede `{prompt}` (so voice content reads first, matching pre-batching adapter UX); images trail `{prompt}` (matching pre-batching adapter UX).

For a single-message dispatch (`batch.len() == 1`) the minimum is two blocks: delimiter + prompt. Each transcript adds one Text block; each image adds one non-Text block. An empty-prompt arrival (e.g. voice-only) skips the prompt block — minimum becomes one delimiter + one transcript.

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

### 3.2 `timestamp` additive field

`SenderContext` JSON gains a `timestamp` field — ISO 8601 UTC, **platform message creation time** (not broker dispatch time):

| Source | Value |
|---|---|
| Discord adapter | `msg.timestamp` (serenity 0.12 `Timestamp`, RFC 3339 by default) |
| Slack adapter | `slack_ts_to_iso8601(event.ts)` — converts epoch-seconds-with-fractional to ISO 8601 with millisecond precision |
| Gateway adapter | `chrono::Utc::now().to_rfc3339()` at receive time — best-effort for non-Discord/Slack channels; documented as approximate |

`schema` stays `openab.sender.v1` — the field is additive and existing parsers keep working. Two purposes:

1. **Distinguishability** — adjacent same-author repetitions become structurally distinct even when other JSON fields would otherwise be byte-identical.
2. **Subsumes `arrived_at_relative` (T2.j)** — the agent computes any relative offset (typing cadence, rapid-fire vs slow correction) directly from absolute timestamps; no separate field needed.

### 3.3 Multi-message batch — concatenate repetitions

For `batch.len() == N` arrival events, the consumer emits the per-arrival template N times back-to-back. **No outer wrapper, no banner, no instruction string, no `<message index=N>` tags.** The next `<sender_context>` opening is itself the boundary marker.

**Example.** Two messages from alice:

- M1 = "look at this" + screenshot
- M2 = audio transcript + "listen to this"

```
Vec<ContentBlock>:
  Text  { "<sender_context>\n{...alice's JSON, timestamp=T1...}\n</sender_context>" }   ← delimiter for M1
  Text  { "look at this" }                                ← M1 prompt
  Image { screenshot }                                    ← M1 attachment
  Text  { "<sender_context>\n{...alice's JSON, timestamp=T2...}\n</sender_context>" }   ← delimiter for M2
  Text  { transcript content }                            ← M2 transcript (precedes prompt)
  Text  { "listen to this" }                              ← M2 prompt
```

Boundary rule: a block belongs to the most recent `<sender_context>` delimiter preceding it; the boundary moves the moment the next `<sender_context>` opens.

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
[TextBlock — transcript content]
listen to this
```

Properties:

- **Attribution is structural via array position** — attachments belong to the most recent `<sender_context>` preceding them in the ContentBlock array. Mirrors Discord's per-message bubble rendering.
- **Multiple attachments per message** group naturally — all of M1's images / transcripts sit between M1's `<sender_context>` and M2's `<sender_context>`, in arrival order.
- **No ACP protocol change.** Still `Vec<ContentBlock>` with existing block types.

### 3.4 Three-way comparison

| Aspect | Current per-message (`adapter.rs:131-152`) | RFC MVP (Appendix A "Packing a batch") | This ADR |
|---|---|---|---|
| Sender attribution | `<sender_context>` JSON wrapper around prompt | New `<message index=N from="…">` attribute (parallel schema) | **Reuse** existing `<sender_context>` JSON verbatim — adds `timestamp` field only |
| Per-batch wrapper | n/a | One combined `Text` block: banner + N `<message>` tags | One delimiter Text block + one prompt Text block + interleaved extras per arrival; no outer wrapper |
| Banner / semantic framing | n/a | `[Batched: N messages…]` always emitted | **None.** No banner, no instruction, no metadata beyond `<sender_context>` |
| Boundary marker | n/a | `<message index=N from="…">` opening + `</message>` close | A standalone `<sender_context>` ContentBlock — the next delimiter opens, the previous arrival ends |
| `<sender_context>` block | Prepended into the same Text block as `{prompt}` | n/a (wholly different schema) | **Standalone** Text block — separate from `{prompt}` block |
| Text extras (transcripts) | Prepended before the combined `<sender_context>+prompt` block (`adapter.rs:138-143`) | Flattened at end of ContentBlock array | Placed after the delimiter but before the `{prompt}` block — voice content reads first |
| Image extras | Appended after main text (`adapter.rs:148-152`) | Flattened at end of ContentBlock array | Appended after the `{prompt}` block (unchanged from pre-batching) |
| Attachment ↔ message link | Implicit (single message) | **Lost** — flattened blocks have no tie back to a sub-message (T1.4 / B1 blocker) | **Structural by adjacency** to the most recent `<sender_context>` delimiter |
| `batch.len() == 1` vs `≥ 2` code paths | Baseline (only path) | Two paths (with/without banner-Text combination) | **Single uniform path** — N=1 is just one repetition of the same template |

### 3.5 Single uniform code path

The packing is one template emitted N times — no special-case fast path for isolated messages. For `batch.len() == 1` the output is one delimiter + transcripts + prompt + images sequence, structurally equivalent to today's per-message dispatch with three small differences:

1. `<sender_context>` JSON now carries a `timestamp` field (additive schema change).
2. `<sender_context>` is its own ContentBlock instead of being prepended into the same Text block as `{prompt}`.
3. STT transcripts move from **before the `<sender_context>` envelope** (today's `adapter.rs:138-143`) to **after the delimiter but before `{prompt}`** — image ordering (after `{prompt}`) is unchanged.

Concretely (Scenario D below): in the current per-message path (`adapter.rs:138-143`), the transcript is prepended before the entire per-arrival template — including `<sender_context>` itself — so it reads as if it were user-typed text:

```
[Voice message transcript]: hey can we sync about the deploy
<sender_context>
{"schema":"openab.sender.v1", ...}
</sender_context>

```

Under this ADR the transcript moves to inside the arrival event (after the delimiter, before `{prompt}`), owned by its arrival event like any other attachment. The boundary rule stays clean: `<sender_context>` always opens an arrival event; transcripts/prompt/images always follow within the same arrival.

### 3.6 Scope of attribution — Scenarios A–D

The packing preserves **structural** attribution (which attachment was uploaded as part of which arrival event). It deliberately does **not** attempt **semantic** attribution (which text refers to which attachment across separate arrival events) — that is exactly the inference that should be left to the ACP agent.

(Sender-context JSON is abbreviated as `{alice, ts=T1}` etc. for readability — in the real ContentBlock stream it's the full JSON record.)

**Scenario A — text and image in the same chat message** (e.g. drag-and-drop with caption)

```
<sender_context>{alice, ts=T1}</sender_context>
look at this
[ImageBlock]
```

The image follows alice's `<sender_context>` with no other `<sender_context>` between → belongs to alice's M1.

**Scenario B — text in one message, image in the next, same author** (very common: type the description, then paste the image)

- M1 (alice): "see this image"
- M2 (alice): [image, no text]

```
<sender_context>{alice, ts=T1}</sender_context>
see this image
<sender_context>{alice, ts=T2}</sender_context>
[ImageBlock]
```

M2 has no `{prompt}` block (empty prompt is omitted, §3.1). Broker keeps the structural truth (image arrived as M2, alone). The agent reads identical `sender_id` on both `<sender_context>` blocks and trivially infers M1's "this image" refers to M2's attachment. The `timestamp` delta `T2 − T1` reinforces this when M1 and M2 are seconds apart.

**Scenario C — fragmented multi-author batch** (alice's text → bob's interjection → alice's image)

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

The broker does not "skip" bob's message or re-link alice's M1 ↔ M3 — those are semantic decisions. The repeated `sender_id=A` lets the agent group by stable user ID across non-adjacent messages; bob's interjection is preserved as-is so the agent can decide whether to address it.

**Scenario D — voice-only message in a batch (existing STT path)**

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

M2 has empty `{prompt}` (so the prompt block is omitted, §3.1) and one transcript block. The transcript lands immediately after the delimiter — within M2's arrival, before any `{prompt}` block would appear.

**Behavior change vs. v0.8.2-beta.1:** in the per-message path (`adapter.rs:138-143`) the transcript is *prepended* before `<sender_context>` so it reads as if it were the user's typed text. Under this ADR the transcript moves to *inside the arrival event*, after the `<sender_context>` delimiter and before `{prompt}`, owned by M2 like any other attachment. The agent still sees the transcript content — just one block down, with the sender envelope explicitly framing it.

**Rollback path if cross-agent smoke fails.** If a Phase 1 cross-agent smoke fixture (Scenario D against Claude Code, Cursor, and Copilot) shows any target regressing on voice-only handling, the response is a code change, not a runtime toggle: either revert the `pack_arrival_event` call for the single-message voice case, or land a hotfix PR re-introducing the `extra_blocks.len() == 1 && prompt.is_empty()` special case that treats the transcript as a `{prompt}` substitute. **No always-on feature flag.** The cross-agent smoke fixture is the gate; a hotfix PR is the rollback mechanism.

The principle (instance of I3): **structural truth is non-negotiable, semantic interpretation is deferred.**

### 3.7 Inbound Discord field fidelity (scope clarification)

Today's broker (`discord.rs:480-483`) extracts only `msg.content` and `msg.attachments` from inbound Discord messages. Other fields — `embeds[]` (including auto-generated link previews), `stickers`, `reactions`, `reference` (reply chain) — are silently dropped. Dispatched ContentBlocks reflect only the fields openab currently ingests; **I3 applies to those fields specifically**. Closing the inbound-fidelity gap is tracked as a follow-up and is out of scope for this ADR.

---

## 4. Configuration & Rollout

### 4.1 Config schema

```toml
[discord]
message_processing_mode = "per-message"  # default in Phase 1
# Or:
message_processing_mode = "per-thread"   # one buffer per (platform, thread)
# Or:
message_processing_mode = "per-lane"     # one buffer per (platform, thread, sender)
max_buffered_messages   = 10             # per-thread / per-lane only; mpsc cap
max_batch_tokens        = 24000          # per-thread / per-lane only; soft cap on cumulative tokens

# Slack and Gateway adapters expose the same three keys under [slack] / [gateway].
```

`message_processing_mode` is **3-valued** (`per-message` / `per-thread` / `per-lane`). All three flow through the same `Dispatcher::submit` path; they differ only in how the dispatcher derives the mpsc identity (`Dispatcher::key`) and what consumer idle timeout it uses (§6.10):

| Mode | mpsc cap | dispatcher key | Idle timeout | When to pick |
|---|---|---|---|---|
| `per-message` | **1** | `(platform, thread_id)` | 10s (`PER_MESSAGE_CONSUMER_IDLE_TIMEOUT`) | Default in Phase 1 — preserves v0.8.2-beta.1 dispatch shape (each message dispatches alone, FIFO via the dispatcher), with the structural changes from §3 (split `<sender_context>` block, transcript ordering). |
| `per-thread` | configured | `(platform, thread_id)` | 300s (`DEFAULT_CONSUMER_IDLE_TIMEOUT`) | One buffer per thread regardless of sender — turn-boundary batching as originally designed. Multiple senders in the same thread share a buffer and produce one ACP turn covering all of them. |
| `per-lane` | configured | `(platform, thread_id, sender_id)` | 300s (`DEFAULT_CONSUMER_IDLE_TIMEOUT`) | One buffer per (thread × sender) — appropriate when peer bots and humans share a thread but their inputs should batch independently. Each sender gets their own mpsc + consumer; all senders still serialize through the shared `Arc<Mutex<AcpConnection>>` per thread. |

**Session pool keying is unchanged across all three modes** — the ACP session is per-thread (`(platform, thread_id)`); only the dispatcher's mpsc identity varies. In `per-lane` mode the per-lane consumers compete for the same connection mutex; per-thread sequential ACP-turn order is preserved by the mutex, while per-lane FIFO order is preserved by each lane's mpsc.

**Why `per-message` still uses the dispatcher (cap=1)** instead of bypassing it: keeping a uniform code path means `cancel_buffered`, sweep, `SendError` recovery (§2.5), and observability (§6.6) work identically across modes — there is no "per-message has its own dispatch path" to maintain. The cap=1 buffer adds μs-scale handle lookup; ACP turn duration dominates by 4–6 orders of magnitude.

**Legacy `"batched"` alias is rejected** — earlier drafts of this ADR used a 2-valued mode (`per-message` / `batched`); configs still using `"batched"` must migrate to either `per-thread` or `per-lane` explicitly. The deserializer rejects unknown values with a clear error so silent fallthrough cannot happen.

### 4.2 Sizing `max_buffered_messages`

The default of 10 covers human-only fragmentation comfortably (typical human typing rate caps at ~3 messages per 30s). For **multi-bot collaboration** channels, however, peer bots can push burst rates well past that. Sampling three multi-bot threads in an early opt-in deployment (~300–1000 messages each, accumulated over several days):

| Thread | Human msgs (max in 30s / 60s) | Peer-bot msgs (max in 30s / 60s) | All incoming (max in 30s / 60s) |
|---|---|---|---|
| Active project thread (~1000 msgs) | 3 / 3 | 12 / 16 | 12 / 16 |
| Status report thread (~360 msgs) | 2 / 3 | 11 / 20 | 11 / 20 |
| Task triage thread (~300 msgs) | 2 / 2 | 24 / 24 | 24 / 24 |

Humans alone never exceeded 3 messages in 30s; peer bots drove all of the burstiness. After this sampling the deployment raised the cap to **30** (~25% headroom over the largest observed 60s burst).

Guidance: human-only adapters use 10; multi-bot adapters size to observed peer-bot burst rate with headroom (typically 20–50). **Backpressure ≠ data loss** — when full, `submit` parks the calling task per-thread; nothing is dropped. Undersizing only produces "more, smaller batches", not lost messages — start at the default and tune up after observing real burst patterns in `dispatch` debug logs.

### 4.3 Sizing `max_batch_tokens`

Default **24000**, sized below typical ACP CLI context budgets with headroom for system prompt + accumulated history. The greedy drain stops when either the count cap or the token cap fires; remaining messages stay in the channel for the next turn (FIFO preserved).

- `BufferedMessage.estimated_tokens` is computed at enqueue from prompt text + extra_blocks; image blocks use a coarse fixed estimate.
- Token estimation is intentionally rough — the goal is a guard rail, not an exact pre-flight. Under-estimating splits a batch that could have fit; over-estimating splits one extra time. Both are recoverable.
- **Splitting only at message boundaries** — never inside a single arrival event. A single oversized message dispatches alone (broker does not split, truncate, or summarize a single arrival event to fit; cf. §6.4 rule 7); the ACP CLI's own context-overflow error surfaces normally.

### 4.4 Phases

```
Phase 1 — Mechanism + T1 dispositions (single PR, Discord + Slack + Gateway)
  - New module: src/dispatch.rs (Dispatcher + ThreadHandle + consumer_loop)
  - pack_arrival_event lives on adapter.rs (single packing path for all modes, §3)
  - 3-valued MessageProcessingMode enum in config (Message / Thread / Lane;
    default = Message)
  - All three modes go through Dispatcher::submit; mode controls
    (cap, BatchGrouping, idle_timeout):
      Message -> (1, BatchGrouping::Thread, PER_MESSAGE_CONSUMER_IDLE_TIMEOUT)
      Thread  -> (max_buffered_messages, BatchGrouping::Thread, DEFAULT_CONSUMER_IDLE_TIMEOUT)
      Lane    -> (max_buffered_messages, BatchGrouping::Lane,   DEFAULT_CONSUMER_IDLE_TIMEOUT)
  - Discord wiring (discord.rs:600-608): unconditional dispatcher.submit()
  - Slack wiring: platform preprocessing moved before dispatcher.submit();
    KeyedAsyncQueue removed — Dispatcher consumer task takes over per-thread serialization
  - Gateway wiring: router.handle_message() replaced with dispatcher.submit()
  - Packing (§3): SenderContext.timestamp additive; pack_arrival_event uniform
    across modes; <sender_context> emitted as standalone Text block (delimiter);
    transcripts placed between delimiter and {prompt}; images placed after {prompt}
  - SendError handling (§2.5): evict + ❌ + ⚠️ + return Err
  - submit does NOT touch last_active (§2.7)
  - other_bot_present per-thread Arc<AtomicBool> mirror (§2.6)
  - Dispatcher::per_thread uses std::sync::Mutex; ThreadHandle.generation: u64
  - sweep_stale: periodic eviction of consumers idle longer than idle_timeout
    (one-shot threads never trigger lazy eviction by themselves; sweep keeps
    HashMap bounded — see §6.10)
  - max_buffered_messages configurable (default 10, multi-bot 30) — per-thread / per-lane only
  - max_batch_tokens soft cap on greedy drain (default 24000) — per-thread / per-lane only
  - Reactions: queued (👀) reaction on ALL messages in batch before dispatch (§6.7);
    applied sequentially (not parallel) to preserve message-ID order in the
    Discord/Slack reaction list; trailing message anchors StatusReactionController progress
  - /cancel-all command + Dispatcher::cancel_buffered (uses generation field)
  - Tracing spans: buffer_wait_ms / agent_dispatch_ms / batch_size (§6.5)
  - SIGTERM: log per-thread buffered count before drop (§6.6)
  - Cross-agent recognition smoke fixture (Claude Code / Cursor / Copilot — Scenario D)
  - SendError manual staging smoke entry (§6.11)

  Tests:
    - per-message mode: single-message dispatch, structurally equivalent to v0.8.2-beta.1
      modulo §3 split-block layout
    - per-thread mode: 3-message fragmentation merges into one batch
    - per-thread mode: new message arrives mid-turn, joins next batch
    - per-lane mode: two senders in same thread → two independent buffers, two
      consumers, but serialized through shared connection mutex
    - per-lane mode: dispatcher key shape is {platform}:{thread}:{sender}
    - /cancel during a batched turn does not drop buffer
    - /cancel-all drops buffered messages and aborts consumer
    - SendError → evict + ❌ + ⚠️ + return Err
    - concurrent SendError → only one eviction; both observers react on own trigger
    - buffer-full → submit parks (no Err, no reaction, no ⚠️)
    - other_bot_present freshness (3-turn timeline)
    - oversized batch (cumulative tokens > cap) splits across two ACP turns; FIFO preserved
    - single message > max_batch_tokens dispatches alone; ACP error surfaces normally
    - voice-only Scenario D pack output
    - queued reaction applied to all batch messages before dispatch (sequential)
    - Scenario B packing: image in separate message from text (same author) — structural adjacency preserved
    - Scenario C packing: multi-author interleaved batch — per-sender attribution preserved across non-adjacent messages
    - per-mode idle timeout: PerMessage consumer evicts after 10s idle; per-thread/per-lane after 300s

Phase 2 — Validation
  - Roll out to a single channel (e.g. dev sandbox)
  - Compare turn counts, latency distributions, user-reported quality
  - Multi-chunk output fan-out under larger batched payloads (split_message line-boundary
    edge cases, placeholder-edit-before-followup ordering, Discord rate-limit headroom,
    chunks_per_response distribution)
  - Per-channel config override ([discord.channels.<id>] for max_buffered_messages,
    message_processing_mode)
  - Gateway per-platform batching validation
  - Per-thread vs per-lane comparison on multi-bot threads (which mode produces
    cleaner ACP output for peer-bot interleaving)

Phase 3 — Default flip (separate RFC if needed)
  - Promote per-thread or per-lane to default (decision deferred to Phase 2 data)
  - Or: keep per-message default if Phase 2 shows no real-world batching wins
```

### 4.5 Adapter integration pattern

All adapters follow a canonical structure:

```
Platform event loop  (single async task, naturally serial)
  ↓
[Before spawn — serial, in event loop]
  bot_turns / gating checks
  ↓
tokio::spawn {
    // 1. Platform-specific preprocessing (parallel across messages, no shared state)
    resolve_user_id() / file download / STT / extra_blocks assembly
    ↓
    // 2. Uniform handoff — platform-agnostic from here
    dispatcher.submit(thread_key, BufferedMessage, ...)
      └─ tx.send() → returns immediately
}

─── consumer_loop (one per active thread) ───
rx.recv() → greedy drain → dispatch_batch()
  ↓
pack_arrival_event() × N → Vec<ContentBlock>
  ↓
pool.with_connection() → conn.lock() → session_prompt()
```

`Dispatcher` is fully platform-agnostic — it only sees `BufferedMessage`, never raw platform events. Platform-specific preprocessing runs in parallel across concurrent messages (no shared mutable state). `bot_turns` and gating checks remain before spawn, in the serial event loop. Future adapters (Telegram, Teams, LINE, etc.) should follow this pattern from the start; `KeyedAsyncQueue` should not be introduced in new adapters.

### 4.6 Migration path

LINE-style atomic cut-over is not required; mode is a per-adapter config flag that can be toggled per channel without external coordination. The conservative Phase 1 default keeps the rollout safe; flipping the default is left to Phase 3 after a validation period.

---

## 5. Alternatives Considered

### 5.1 Mechanism alternatives

**Per-message dispatch (status quo, v0.8.2-beta.1).** The baseline. Each arrival becomes its own ACP turn. **Rejected as the steady state** because turns 1..N-1 may waste work (intermediate output, redundant tool invocations) before turn N arrives with the corrected intent — which is exactly the workload §1.1 documents. Retained as the Phase 1 default and as the per-message mode of the adapter config flag for safe rollback.

**Pre-turn debouncing.** Wait `debounce_ms` after each message before dispatching (e.g. Hermes' adapter-level `_pending_text_batches`, ~0.6–2.0s). **Rejected** because it imposes a latency floor on every message including isolated ones, violating I1. The buffering-during-turn approach pays zero added latency for isolated messages because the turn duration itself is the natural buffering window, used for free.

**Single-slot in-flight overwrite (Hermes pattern).** When a new message arrives during an in-flight turn, overwrite the previous pending message and signal an interrupt to the agent loop. **Rejected on two counts.** First, it drops messages: M2 is overwritten by M3 on rapid-fire bursts, with no recovery. Second, it requires mid-turn interrupt of the agent — possible for Hermes / OpenClaw because their agent loop is in-process (asyncio), but **not possible for openab** because the agent is an external ACP CLI (Claude Code, Cursor, Codex) that yields control only at turn end. This is an architectural constraint, not a design choice.

**Mid-turn interrupt.** Cancel the in-flight ACP turn when a new message arrives, restart with combined context. **Rejected — same architectural constraint as above.** External ACP CLIs do not expose an interrupt protocol that yields control between tool calls; `/cancel` aborts at turn boundary, not mid-stream.

**Topic detection / semantic grouping in broker.** Apply rules ("same user + < 3s gap = merge") or an LLM classifier to decide which messages to merge into one ACP turn. **Rejected — violates I3.** Real grouping is *semantic* (was message N+1 a continuation of N's intent, an unrelated topic, a correction?); the broker has no way to answer that without an LLM, and the user's ACP session **already has** the full context and is the right place to make semantic decisions.

**Per-channel or global buffer keying.** Aggregate messages across threads in a channel (or globally across channels). **Rejected** because conversation scope in openab = thread; per-thread keying matches the existing `Arc<Mutex<AcpConnection>>` keying. Cross-thread merging would conflate independent conversations.

**HTTP-style request coalescing in the per-connection mutex itself.** Retrofit batching onto the Tokio Mutex's waker list. **Rejected** because Tokio Mutex wakers are opaque: the mutex sees only "someone is waiting" and cannot enumerate pending messages, inspect content, or batch them. Batching requires an explicit queue at a layer that owns the message data — that's the dispatcher.

### 5.2 Packing alternatives

**RFC MVP wrapper-and-flatten** — wrap each sub-message text in `<message index=N from="…">…</message>` and flatten all sub-messages' `extra_blocks` into a single tail of the ContentBlock array. **Rejected** for two failures: (1) attribution loss (T1.4 / B1) — image and transcript blocks at the tail have no tie back to a `<message index=N>`, so the agent can't know which image belongs to which sub-message; (2) parallel sender-encoding schemes — `from="alice"` duplicates information already in `<sender_context>` JSON's `display_name` and risks drift if one schema evolves and the other doesn't.

**RFC MVP wrapper, `extra_blocks` placed inside the `<message>` tag.** A patch on the above: place each sub-message's `extra_blocks` immediately after its `<message index=N>` tag (JARVIS's suggested fix). **Rejected** because the same fix is achievable using `<sender_context>` itself as the boundary marker — no need to introduce a parallel `<message>` schema. §3's design is the same fix expressed without the new wrapper tag.

**Keep current asymmetric ordering as a special case.** Preserve `adapter.rs:138-152` ordering via an `extra_blocks.len() == 1 && prompt.is_empty()` branch on every single-message dispatch. **Rejected.** Single uniform code path beats a fast-path branch for a marginal Scenario D readability difference. Scenario D's behavior change is reversible if cross-agent smoke shows real disruption (§3.6 rollback).

**Inject a leading `[Batched: N messages…]` banner string.** **Rejected — violates I3.** Broker injecting framing is a semantic directive ("treat these as one logical unit") that the agent can no longer un-see. Whether to treat the messages as one logical unit is the kind of judgment the agent should make from the structural facts (same `sender_id`, close `timestamp` deltas), not from a broker hint.

**Sidecar metadata block (JSON map).** Single trailing JSON block describing per-arrival attribution — e.g. `{"events":[{"index":0,"sender_id":"A","ts":"…","attachment_indices":[2,3]}, …]}` — appended once at the end of the ContentBlock array, with all `<sender_context>` headers removed and prompts concatenated. **Rejected** for three reasons: (1) single-sequence readability — pushing attribution into a tail JSON forces the agent to cross-reference `attachment_indices` against array positions, losing the affordance that adjacency provides for free; (2) parser coupling — introduces a second schema, duplicating the failure mode of the parallel `<message>` tag; (3) ACP / tool-use mismatch risk — some agents may treat trailing JSON as a tool-result fragment or post-prompt instruction.

### 5.3 Prior art

Two adjacent systems solve "user typed multiple times in quick succession" with different trade-offs. Both are personal AI agent runtimes (single-user, agent loop bundled into the gateway process) — different shape from openab's multi-tenant broker, but the in-flight buffering problem is the same.

| Aspect | Hermes Agent | OpenClaw | Current openab | This ADR |
|---|---|---|---|---|
| Shape | Single-user runtime, gateway = agent | Single-user runtime | Multi-tenant broker → external ACP CLI | Same as current |
| First-message latency | ~0.6–2.0s (Discord adapter debounce — API split compensation, not user-intent batching) | n/a observed | **0** (immediate dispatch) | **0** (preserved) |
| Adapter-level batching | `_pending_text_batches`, `_text_batch_split_delay_seconds` — reassembles >2000-char Discord-auto-chunked messages | `extensions/discord/src/monitor/message-handler.ts` (preflight debounce only — not in-flight turn-boundary batching) | None | None (deliberate) |
| In-flight new message | Single-slot `_pending_messages[key]` — **overwrites prior** + `interrupt_event.set()` cancels in-flight | n/a observed | N independent `tokio::spawn` tasks each park on per-thread mutex | Send to per-thread bounded `mpsc`; consumer batches at turn boundary |
| Buffer data structure | `Dict[str, MessageEvent]` (1 slot) | — | None (mutex waker list, opaque) | bounded `mpsc::channel` (FIFO, default cap 10) |
| 3 fast messages → ACP turns | **1 turn**, middle message dropped by overwrite | — | **3 turns**, intermediate output wasted | **2 turns** (M1, then batch [M2, M3]) — no message lost |
| Mid-turn interrupt | **Yes** (asyncio interrupt event) — agent loop is in-process | — | No | No |
| Same-thread message ordering | (1-slot makes this moot) | — | Approximate (Tokio Mutex not strictly FIFO) | Strict (mpsc FIFO) |
| Per-key serialization | `asyncio.Event` + `_active_sessions` dict | `src/plugin-sdk/keyed-async-queue.ts` | `KeyedAsyncQueue` (per-key Semaphore, Slack) + `Arc<Mutex<AcpConnection>>` | Inherited |
| Bot-message gating | `DISCORD_ALLOW_BOTS` (off / mentions / all) | n/a observed | `allow_bot_messages` (3-value, borrowed from Hermes) | Inherited |
| Stall UX feedback | — | `extensions/discord/src/monitor/inbound-worker.ts` | `reactions.rs` stall_soft / stall_hard (borrowed from OpenClaw) | Inherited |

**Three trade-off axes:**

1. **Drop vs preserve.** Hermes' single-slot overwrite drops middle messages in fast bursts; openab (current and ADR) preserves all.
2. **Interrupt vs wait for boundary.** Hermes can interrupt the in-flight LLM call because the agent loop is in-process. openab cannot — the agent is an external ACP CLI that yields control only at turn end. This is an *architectural* constraint, not a design choice. The ADR turns it into a feature: the existing turn duration is the natural buffering window, with no added latency for isolated messages.
3. **Debounce vs piggyback.** Hermes' Discord adapter pays ~0.6–2.0s per message regardless (for API split compensation). The ADR pays 0 for isolated messages — buffering only fills *during* an active turn, when the user is already waiting on the agent.

**Conclusion:** Neither Hermes nor OpenClaw implements turn-boundary batching. This ADR's design is novel among these three systems — it turns the architectural constraint (no mid-turn interrupt for external ACP CLIs) into a feature (zero-latency first message + lossless FIFO buffering during active turns).

**Note on OpenClaw source paths:** `stall-watchdog.ts` does not exist in the current OpenClaw repo — stall handling lives in `extensions/discord/src/monitor/inbound-worker.ts`.

HTTP request coalescing in proxies (Varnish, nginx) — same shape ("while one request is in flight, batch / dedupe others") in a different domain.

---

## 6. Consequences & Compliance

### 6.1 Positive

- **First-message latency unchanged at zero** — I1 preserved in steady state.
- **Wasted intermediate output eliminated** for fragmented input — turn 1's full output + tool execution before turn 3 supersedes it never gets generated. Saved tokens scale with input fragmentation.
- **Deterministic same-thread ordering** — strict FIFO via per-thread `mpsc::channel` replaces the not-strictly-FIFO Tokio Mutex waker list.
- **Attachment attribution recoverable by adjacency** (§3) — closes T1.4 / B1 with one structural change.
- **No new packing schema invented.** Reuses `<sender_context>` (already known to every ACP agent that consumes today's per-message format) plus one additive `timestamp` field. `schema` stays `openab.sender.v1`.
- **Subsumes T2.b** (`sender_name` disambiguation) — `sender_id` is already in `<sender_context>` JSON.
- **Subsumes T2.j** (`arrived_at_relative` offset) — agent computes any relative offset from absolute `timestamp`s.
- **Single uniform packing code path.** N=1 and N≥2 share the exact same packer.
- **No ACP protocol change.** Still `Vec<ContentBlock>` with existing block types.
- **Validated end-to-end on a staging deployment** (2026-04-27, k9). Per-arrival shape and `timestamp` field confirmed under organic traffic; multi-message batch concatenation (`batch_size=2`) confirmed to produce a single streaming-edit reply per batch.

### 6.2 Negative

- **Multi-message batches lower the ACP-turn count visible to ops.** `bot_turns` ingest counter (`slack.rs:672-696`) runs before the dispatcher, so per-message loop limits still fire correctly. The downstream "ACP turns" metric shifts to count batches; document it.
- **`/cancel` cancels one ACP turn that may now contain multiple user messages.** Documented: "cancel current ACP work; buffered messages fire next." `/cancel-all` covers drop-on-cancel (Phase 1).
- **§2.7 zombie blast radius widens under batching** until the dedicated follow-up lands. Phase 1 is no-worse-than-released (it does not amplify the bug, but does not fix the underlying `cleanup_idle.try_lock` skip either).
- **Scenario D regression even in `per-message` mode.** STT voice transcripts move from prepended-before-`<sender_context>` to placed-between-delimiter-and-prompt, changing the read order for single-message voice dispatches. The change is structural (it affects all three modes — `per-message` included — because all share the §3 packing path). Reversible via a special case if cross-agent smoke shows real disruption.
- **`{prompt}` empty case is structurally valid.** Voice-only / attachment-only messages produce an empty line between `</sender_context>` and the first attachment block. Agents that strictly validate "non-empty prompt" need to relax that assumption — but this is already the case for any voice-only message under today's format.
- **Cross-agent recognition risk.** Multi-`<sender_context>` repetition is a new shape from the agent's perspective. Existing single-`<sender_context>` parsing should generalize naturally (it's just the same envelope opening twice), but Phase 1 includes a manual cross-agent smoke fixture against Claude Code, Cursor, and Copilot.
- **Token-cost surface widens.** Each repetition re-emits the full `<sender_context>` JSON. For multi-bot channels with `max_buffered_messages = 30`, the per-batch envelope overhead is non-trivial. `max_batch_tokens` (default 24000) bounds total batch size — drain stops when either count or token cap fires, splitting only at message boundaries.

### 6.3 Neutral

- **`<sender_context>` proliferation in agent-visible context.** The agent now sees N `<sender_context>` blocks per batched turn instead of one. This is the intended structural fact, not noise — agents that previously parsed exactly one block per turn need to handle the N≥2 case, but the parsing rule is the same.
- **`timestamp` is wall-clock visible.** Discord/Slack already display the same timestamps to all participants in the channel; this is not a new exposure.
- **Behavior change observable to every user of an opted-in channel.** Mitigated by per-adapter opt-in for v1; default flip deferred to Phase 3 after validation.

### 6.4 Compliance rules

The rules below operationalize I3 (broker structural fidelity). Together they form the test surface that any future packing or dispatch change is judged against.

1. **Broker forwards `{prompt}` verbatim.** Broker must not parse, classify, transform, summarize, or annotate the user-supplied text content within `{prompt}`. Any future feature that needs to inspect `{prompt}` content must do so without mutating what the agent receives.

   **Counter-examples (prohibited):** broker stripping markdown formatting before dispatch; broker expanding Discord `<@123>` mentions to `@username` strings; broker appending an `[image attached]` string when an image accompanies the prompt; broker collapsing repeated whitespace; broker normalizing Unicode forms.

2. **No banners or framing strings.** Broker must not inject any leading or trailing instruction text into the dispatched batch (e.g. no `[Batched: N messages…]`, no `[End of batch]`). All metadata lives in `<sender_context>` JSON.

3. **No wrapper tags beyond `<sender_context>`.** Multi-message batches are produced by repeating the per-arrival template; no `<message>`, `<batch>`, or other wrapper schema is introduced. Future schema needs are extended as additive fields inside `<sender_context>` JSON, not as new XML tags.

4. **Attachment attribution is structural via array position.** Broker must place each arrival event's `extra_blocks` between that event's `<sender_context>` delimiter and the next event's `<sender_context>` delimiter (i.e. inside the same arrival event), in the same order they were received from the platform adapter. Within an arrival, Text `extra_blocks` precede `{prompt}` and non-Text `extra_blocks` follow `{prompt}` (§3.1). No reordering, no deduplication, no cross-arrival re-attribution.

   **Counter-examples (prohibited):** broker sorting `extra_blocks` by type (e.g. all images first, then transcripts); broker hoisting all images of a batch to a "gallery" section at the end; broker deduplicating two identical images sent in the same batch; broker re-attributing M2's image to M1 because M1 had text and M2 was image-only.

5. **`SenderContext` schema is additive.** New fields may be added under the `openab.sender.v1` name; field removal or semantic change requires a `v2` bump and a migration path for downstream agents.

6. **`timestamp` is platform message creation time when available.** Discord and Slack adapters must use the platform's own message creation timestamp. The gateway adapter's receive-time fallback must be documented as best-effort to downstream consumers.

7. **Splitting only at message boundaries.** When the token-budget cap (`max_batch_tokens`) forces a batch to split across multiple ACP turns, the split must occur between two arrival events — never inside a single arrival event. A single oversized message dispatches alone; the broker does not truncate or summarize it.

8. **No silent failure on consumer death.** When `submit` observes `SendError` (consumer task death), the failure must surface as ❌ on `msg.trigger_msg` **and** `⚠️ {format_user_error}` text in the channel **and** `Err` propagated to the caller. Already-enqueued messages whose `submit` already returned `Ok` are residual loss equivalent to a pod restart mid-turn (documented; out of Phase 1 scope to recover). Messages in the consumer's in-flight batch at the time of the panic are also residual loss — their `submit` already returned `Ok` before the consumer died, so they cannot be reacted from the `SendError` path.

9. **`bot_turns` runs at ingest, not at dispatch.** Multi-bot loop guards (`slack.rs:672-696`) execute before `submit`; batching is downstream and cannot bypass them. Bot-turn-limit counts batches as turns (one ACP invocation = one logical turn); the per-message ingest counter is unchanged.

### 6.5 Semantic neutrality — prohibited transformations

The following classes of transformation are categorically forbidden because they make semantic judgments the broker is not authorized to make. They are listed explicitly so future "small optimization" PRs can be rejected by reference rather than re-litigated:

- **No topic split.** Broker must not split a single arrival event into multiple ACP turns based on content (e.g. detecting "two questions in one message"). One arrival = one event in the dispatched batch.
- **No intent merge.** Broker must not coalesce two adjacent same-sender messages into a single event even when they appear to express one logical thought ("see this" + "[image]"). Each arrival keeps its own `<sender_context>`.
- **No sender collapse.** Broker must not merge multiple distinct `sender_id`s into a single header even when display names or roles match (e.g. two human users with the same name, or two bots with the same role). Each unique sender event gets its own `<sender_context>`.
- **No silent drop.** Broker must not omit an arrival event from a batch on the grounds that it appears redundant, off-topic, or empty. The agent decides what to do with it.
- **No ordering inversion.** Broker must not reorder events within a batch based on perceived priority, sender role, or content type. Arrival order from the platform adapter is preserved.

If a future feature genuinely requires one of these transformations, it belongs in the ACP agent (which has the semantic context to make the call), not in the broker. The broker's job ends at faithful structural transport.

### 6.6 Observability

Phase 1 emits **one `info!` line per dispatch** carrying both per-dispatch and per-event values as structured fields — no new dependencies, no JSON formatter change, no `metrics` crate. Default `EnvFilter = openab=info` means these appear in production logs without env var changes.

| Metric | Emit point | Notes |
|---|---|---|
| `events_per_dispatch` | per dispatch | Downstream computes 1h-rolling `p95_batch_size` from this stream |
| `packed_block_count` | per dispatch | Total `ContentBlock` count emitted to ACP |
| `agent_dispatch_ms` | after `dispatch_batch` returns | dispatch → ACP turn completion latency |
| `context_tokens_per_event` | per dispatch (as array field) | `tokens_per_event=[123,145,98]`; downstream reconstructs distribution |
| `buffer_wait_ms` | per dispatch (as array field) | `wait_ms=[42,38,0]`; per-sub-message enqueue → dispatch latency |

```rust
info_span!("dispatch", channel = %channel_id, adapter = "discord")
    .in_scope(|| {
        info!(
            events_per_dispatch = batch.len(),
            packed_block_count  = blocks.len(),
            agent_dispatch_ms   = elapsed.as_millis(),
            tokens_per_event    = ?per_event_tokens,
            wait_ms             = ?per_event_wait_ms,
            "batch dispatched",
        );
    });
```

Per-event metrics fold into the per-dispatch line as array fields → log line count = dispatch count, independent of batch size.

**Threshold for dedup re-evaluation:** when `p95_batch_size × avg_tokens_per_event > 500 tokens` (used as a rough proxy for per-dispatch `<sender_context>` envelope overhead) on any production channel for a sustained 24h window, the broker team must re-open the dedup question (e.g. emit `<sender_context>` only when sender or timestamp delta changes). Below that threshold the envelope cost is below noise and the readability win of always-explicit headers wins.

**Phase 1 acceptance test (masami #1):** after Phase 1 lands and is deployed to a test channel, send a 3-message batch and verify the single `info!` line carries `events_per_dispatch = 3`, `packed_block_count = N`, `agent_dispatch_ms = N`, `tokens_per_event = [t1, t2, t3]`, `wait_ms = [w1, w2, w3]`. If any field is missing or events are split across multiple log lines, Phase 1 does not merge.

### 6.7 Batch reaction UX

In batched mode, every message in a batch (including non-trailing messages) gets an `emojis.queued` (👀) reaction before `session_prompt` is called. This prevents the "message eaten" perception where the first message in a batch sits silent for 30+ seconds with no feedback.

```rust
// in dispatch_batch(), before session_prompt
for msg in &batch {
    let _ = adapter.add_reaction(&msg.trigger_msg, &reactions_config.emojis.queued).await;
}
// StatusReactionController still anchors on batch.last().trigger_msg as before
```

Applies to all batches including `batch.len() == 1` — the loop runs once for a single-message batch, making the reaction path uniform and removing the need for a separate `set_queued()` call in `StatusReactionController` for the single-message case. The reactions are fire-and-forget (`let _ =`); failures are silently ignored, same as existing reaction calls throughout the codebase. The 👀 reactions are not removed after dispatch — they serve as permanent "received" markers.

**Sequential, not parallel.** The loop applies reactions one at a time (`for ... await`), not concurrently via `join_all`. Both Discord and Slack append reactions in the order the API receives them; parallel `join_all` would race-order the appends and produce visually inconsistent reaction-list ordering across the batch (M3's reaction may land before M1's in the API's view). The serial latency cost is bounded — each `add_reaction` is one HTTP round-trip (≤200ms typical), and a full burst-cap-30 batch is ≤6s — small relative to the ACP turn duration this is gating against.

### 6.8 Graceful shutdown

On `SIGTERM`, `Dispatcher::shutdown()` iterates active threads and logs `thread_id=…, channel=…, adapter=…, buffered_lost=N` per thread before drop. `ThreadHandle` carries `channel_id: String` and `adapter_kind: String` (set at consumer-spawn time) so the shutdown log can identify which channel lost messages without iterating `BufferedMessage` contents.

```rust
// in Dispatcher::shutdown()
let mut map = self.per_thread.lock().unwrap();
for (thread_id, handle) in map.iter_mut() {
    // drain_pending requires &mut ThreadHandle to close the sender
    let pending = handle.drain_pending();
    if !pending.is_empty() {
        warn!(
            thread_id     = %thread_id,
            channel       = %handle.channel_id,
            adapter       = %handle.adapter_kind,
            buffered_lost = pending.len(),
            "shutdown drained pending messages without dispatch",
        );
    }
}
```

`Dispatcher::shutdown()` is placed after adapter handles are joined and before `pool.shutdown()` in the `main.rs` cleanup sequence. It is synchronous (`std::sync::Mutex` + synchronous drain) — no `await`, no timeout required.

Buffered state is in-memory only; pod restart loses it by design (no on-disk WAL, no Redis-backed buffer). Ops decide per `buffered_lost > 0` event whether to ask users to re-send.

### 6.9 Scenario D smoke matrix (Phase 1 must-do)

Cross-agent smoke verifies that agents correctly read transcript content after the ordering change (transcript moves from before `<sender_context>` to inside the arrival event, between the `<sender_context>` delimiter and `{prompt}`).

**Prerequisites:** `stt.enabled = true`, `GROQ_API_KEY` set, Discord mobile to send voice messages (Discord desktop does not support voice message recording), `audio/ogg` MIME passing `media::is_audio_mime()`.

| Agent | Test case | Pass criteria |
|---|---|---|
| Claude Code | Voice-only message in a thread → agent responds | Reply text references / acknowledges transcript content (not just emoji or "got it") |
| Claude Code | Voice + text in same batch → agent responds | Reply addresses both the typed text **and** the voice transcript |
| Cursor | Voice-only message in a thread → agent responds | Same as Claude Code voice-only |
| Copilot | Voice-only message in a thread → agent responds | Same as Claude Code voice-only |

**Decision gate:** if any agent fails to reference transcript content, do not merge Phase 1. Apply the `extra_blocks.len() == 1 && prompt.is_empty()` escape hatch (§3.6 rollback), re-run the matrix. If still failing: hold Phase 1, file follow-up.

### 6.10 Per-mode consumer idle timeout

Each `Dispatcher` carries an `idle_timeout: Duration` chosen per `MessageProcessingMode` (§4.1). The consumer evicts itself when no message arrives within `idle_timeout`; eviction drops the `ThreadHandle` and closes the mpsc, after which the next `submit` lazily constructs a fresh handle through the same map-insert path used at first arrival.

Two named constants:

```rust
pub const PER_MESSAGE_CONSUMER_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_CONSUMER_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
```

| Mode | Idle timeout | Rationale |
|---|---|---|
| `per-message` | 10s | The buffer is cap=1, so each consumer drains exactly one message per turn. Once the turn completes there is no batching window to preserve — keeping the consumer alive is pure overhead. A 10s timeout absorbs consecutive rapid-fire messages without the cost of repeatedly re-spawning the consumer task, while still freeing the slot quickly when the user goes idle. |
| `per-thread` / `per-lane` | 300s | The buffer fills *during the turn* (I1) and the consumer needs to be there at turn completion to drain. After the turn, the consumer parks on `recv()` waiting for follow-up messages from the same thread / lane. Five minutes is long enough to absorb typical user-thinking-then-typing pauses while still bounding the idle resource footprint. |

**Why not zero / one-shot for `per-message`.** A consumer-per-message lifecycle would re-spawn `tokio::Task` + re-allocate the handle on every arrival, doubling the dispatcher overhead per message. For rapid-fire bursts (the workload §1.1 documents) this is the wrong direction. 10s gives the consumer a chance to handle the burst as a sequence of cap=1 dispatches with one task spawn.

**Why not 300s for `per-message`.** Per Little's Law (`L = λ × W`), an N-thread system at λ messages/min/thread × 5min idle window yields ~30× more idle consumer tasks than a 10s window. For batched modes the long window is paying for batching opportunity; for `per-message` it is paying for nothing.

**Sweep eviction.** `Dispatcher::sweep_stale` runs periodically (called from `main.rs`) to evict consumers that have been idle past `idle_timeout`. This is required because lazy eviction (consumer self-times-out on `recv` and exits) only fires when the consumer is parked — for one-shot threads where a single message arrives and the user never returns, the consumer would otherwise live until the process exits. Sweep keeps the `per_thread` HashMap bounded at the cost of one synchronous lock + a HashMap iteration per tick.

### 6.11 SendError manual staging smoke matrix (Phase 1 must-do)

Automating an end-to-end SendError test is awkward because the failure path requires a panic inside a live consumer task — which is hard to inject deterministically in CI without making `dispatch.rs` test-flag-aware. SendError handling (§2.5) therefore validates via a manual staging smoke run; this section is the matrix that run uses.

**Prerequisites:** staging deployment with `RUST_LOG=openab=debug` (so `dispatch` debug events show up); a thread the operator owns; ability to attach to the pod (`kubectl exec`) to send a `SIGUSR1`-style panic — or, alternatively, deploy a build that injects a one-shot panic via env var.

| Step | Action | Pass criteria |
|---|---|---|
| 1 | Send M1 in a fresh thread; wait for the agent to start a turn (👀 reaction visible). | Consumer task is running, `<thread_key>` appears in `per_thread` map. |
| 2 | While the turn is in flight, send M2 and M3. | Both arrivals get the 👀 reaction; both are in the consumer's mpsc buffer. |
| 3 | Trigger a panic inside the consumer task (e.g. via injected one-shot panic, or `pkill` the agent process so `session_prompt` returns Err and the consumer panics). | Consumer task exits; the existing `<thread_key>` entry is now stale. |
| 4 | Send M4 in the same thread. | M4's `submit` observes `SendError` and: (a) ❌ reaction lands on M4's trigger message; (b) `⚠️ {format_user_error}` text is sent to the channel; (c) the dispatcher map entry is evicted; (d) submit returns `Err`. |
| 5 | Send M5 in the same thread. | A fresh consumer is lazily constructed under the same `<thread_key>`; M5 dispatches normally. M2 / M3 / M4 are not recovered (residual loss, §2.5). |
| 6 | Trigger SendError concurrently from two messages (script that sends two messages in <50ms after the consumer has died). | Only one eviction happens (verify in `dispatch` debug logs). Both messages get their own ❌ + `⚠️` (anchored to their own `trigger_msg`). |

**Decision gate:** all six rows must pass on staging before Phase 1 ships. Failures fall into two classes:

- **(a)–(d) at step 4 partial fail** — e.g. ❌ lands but `⚠️` text doesn't, or eviction doesn't happen. Hold Phase 1, fix, re-run.
- **Step 6 double-eviction** — eviction is supposed to be race-safe via the generation counter (§2.5). Hold Phase 1, debug the `per_thread` lock + generation field, re-run.

---

## Appendix A: Reference Implementation

This sketch matches the MVP shape in `src/dispatch.rs`. Signatures carry extra parameters not present in earlier RFC drafts; the rationale is inline.

> **Sketch caveat:** emoji references in this appendix (`Emoji::Queued`, `Emoji::Error`) are simplified for readability. Actual code uses `reactions_config.emojis.queued` / `reactions_config.emojis.error` from the config struct.

```rust
use std::time::Instant;
use std::sync::Mutex;
use tokio::sync::mpsc;

struct BufferedMessage {
    prompt: String,
    extra_blocks: Vec<ContentBlock>,
    sender_json: String,
    trigger_msg: MessageRef,
    arrived_at: Instant,
    /// Display name for inline batch labelling. Not a stable user ID.
    sender_name: String,
    estimated_tokens: usize,
}

struct ThreadHandle {
    tx: mpsc::Sender<BufferedMessage>,
    _consumer: tokio::task::JoinHandle<()>,
    /// Race-safe eviction counter (§2.5). Plain u64 — all reads/writes under per_thread lock.
    generation: u64,
    channel_id: String,
    adapter_kind: String,
}

pub struct Dispatcher {
    /// std::sync::Mutex — critical section has no .await; tokio::Mutex buys nothing here.
    per_thread: Mutex<HashMap<String, ThreadHandle>>,
    router: Arc<Router>,
    max_buffered_messages: usize,
    max_batch_tokens: usize,
}

impl Dispatcher {
    /// `adapter` and `other_bot_present` are passed per-call (rather than stored
    /// on the Dispatcher) because the Discord adapter is constructed inside
    /// serenity's `ready` callback via `OnceLock` — well after the Dispatcher
    /// itself is built in `main.rs`. Per-call passing avoids that chicken-and-egg.
    pub async fn submit(
        &self,
        thread_key: String,
        thread_channel: ChannelRef,
        adapter: Arc<dyn ChatAdapter>,
        msg: BufferedMessage,
        other_bot_present: Arc<AtomicBool>,
    ) -> Result<(), DispatchError> {
        let cap = self.max_buffered_messages;
        let router = Arc::clone(&self.router);
        let max_tokens = self.max_batch_tokens;

        let (tx, my_generation) = {
            let mut map = self.per_thread.lock().unwrap();
            let entry = map.entry(thread_key.clone()).or_insert_with(|| {
                let (tx, rx) = mpsc::channel(cap);
                let consumer = tokio::spawn(consumer_loop(
                    thread_key.clone(), thread_channel.clone(), rx, router,
                    Arc::clone(&adapter), cap, max_tokens,
                    Arc::clone(&other_bot_present),
                ));
                ThreadHandle {
                    tx,
                    _consumer: consumer,
                    generation: 0,
                    channel_id: thread_channel.channel_id.clone(),
                    adapter_kind: adapter.kind().to_string(),
                }
            });
            (entry.tx.clone(), entry.generation)
        };
        // dispatcher mutex released — held only to look up/insert the handle

        if let Err(e) = tx.send(msg).await {
            // Consumer has exited — race-safe eviction under lock
            let mut map = self.per_thread.lock().unwrap();
            if let Some(handle) = map.get(&thread_key) {
                if handle.generation == my_generation {
                    map.remove(&thread_key);
                }
            }
            // Surface error to user (§2.5)
            let failed_msg = e.0;
            adapter.add_reaction(&failed_msg.trigger_msg, &Emoji::Error).await;
            adapter.send_message(
                &thread_channel,
                format!("⚠️ {}", format_user_error("dispatch consumer exited unexpectedly")),
            ).await;
            return Err(DispatchError::ConsumerDead);
        }
        Ok(())
    }

    pub fn shutdown(&self) {
        let mut map = self.per_thread.lock().unwrap();
        for (thread_id, handle) in map.iter_mut() {
            // sketch: drain_pending requires &mut ThreadHandle to close the sender
            // and collect remaining BufferedMessages from the channel
            let pending = handle.drain_pending();
            if !pending.is_empty() {
                warn!(
                    thread_id     = %thread_id,
                    channel       = %handle.channel_id,
                    adapter       = %handle.adapter_kind,
                    buffered_lost = pending.len(),
                    "shutdown drained pending messages without dispatch",
                );
            }
        }
    }
}

async fn consumer_loop(
    _thread_key: String,            // reserved for tracing spans
    thread_channel: ChannelRef,
    mut rx: mpsc::Receiver<BufferedMessage>,
    router: Arc<Router>,
    adapter: Arc<dyn ChatAdapter>,
    max_batch: usize,
    max_tokens: usize,
    other_bot_present: Arc<AtomicBool>,
) {
    // pending holds a message that was dequeued but exceeded the token cap for the
    // current batch; it becomes the first message of the next batch, preserving FIFO.
    let mut pending: Option<BufferedMessage> = None;
    loop {
        let first = match pending.take() {
            Some(msg) => msg,
            None => match rx.recv().await {
                Some(msg) => msg,
                None => break,
            },
        };
        // Greedy drain up to max_batch messages or max_tokens
        let mut batch = vec![first];
        let mut cumulative_tokens = batch[0].estimated_tokens;
        while batch.len() < max_batch {
            match rx.try_recv() {
                Ok(more) => {
                    if cumulative_tokens + more.estimated_tokens > max_tokens {
                        // Token cap reached — save for next turn instead of dropping.
                        // mpsc has no unget; pending slot carries it across the loop boundary.
                        pending = Some(more);
                        break;
                    }
                    cumulative_tokens += more.estimated_tokens;
                    batch.push(more);
                }
                Err(_) => break,
            }
        }

        // Read freshness at dispatch time (§2.6)
        let bot_present = other_bot_present.load(Ordering::Relaxed);

        // Apply queued reaction to all messages in batch (§6.7)
        for msg in &batch {
            let _ = adapter.add_reaction(&msg.trigger_msg, &Emoji::Queued).await;
        }

        dispatch_batch(
            &router, &adapter, &thread_channel,
            batch, bot_present,
        ).await;
    }
    // rx.recv() returned None → all senders dropped → cleanup_idle evicted us. Exit cleanly.
}
```

---

## Notes

- **Version:** 0.3
- **Changelog:**
  - 0.3 (2026-04-30): Merge all RFC source documents and issue #580 comments into single ADR. Additions: §2.3 `generation: u64` + `channel_id`/`adapter_kind` on `ThreadHandle`; §2.5 race-safe eviction detail and action chain (T1.1); §2.7 axis-1/axis-2 analysis and related issue inventory (T1.2); §3.4 three-way comparison table (packing-adr §4); §4.4 Slack/Gateway Phase 1 wiring and canonical adapter integration pattern (tier2-roundup §4); §4.5 adapter integration pattern; §5.3 Prior Art detailed comparison table with Hermes/OpenClaw source-level analysis (rfc-turn-boundary.md); §6.5 semantic neutrality prohibited transformations (packing-adr §8.1); §6.6 observability three-metric spec with acceptance test (T2.a / masami #1); §6.7 batch reaction UX Phase 1 (T2.h); §6.8 graceful shutdown design (T2.g); §6.9 Scenario D smoke matrix (masami #2); Appendix A reference implementation (rfc-turn-boundary.md Appendix A, updated for final design).
  - 0.2 (2026-04-29): Restructure per maintainer feedback — collapse to 6 decision-focused sections; T1.x dispositions become inline rationale, no longer chapter spine; add §5.1 mechanism alternatives (debounce / Hermes overwrite / mid-turn interrupt / topic detection / cross-thread keying / mutex-coalescing); strip RFC-process narrative; anchor pinning simplified to v0.8.2-beta.1 (`52052b8`) for all file:line refs.
  - 0.1 (2026-04-29): Initial proposed version. Folds RFC #580 mechanism, T1.1 / T1.2 / T1.3 resolutions, and the standalone packing ADR (PR #598) into a single document.

## References

- [RFC #580: Turn-boundary message batching](https://github.com/openabdev/openab/issues/580) — kept as historical discussion record.
- [PR #598 (superseded): docs(adr): batched turn packing in ACP session/prompt](https://github.com/openabdev/openab/pull/598) — standalone packing ADR; folded into §3 of this document.
- [Triage T1.1 / T1.2 standalone comment (#issuecomment-4338125509)](https://github.com/openabdev/openab/issues/580#issuecomment-4338125509) — SendError + last_active disposition.
- [Triage T1.3 standalone comment (#issuecomment-4329250043)](https://github.com/openabdev/openab/issues/580#issuecomment-4329250043) — `other_bot_present` freshness.
- [Triage T1.4 + B1 packing comment (#issuecomment-4325645814)](https://github.com/openabdev/openab/issues/580#issuecomment-4325645814) — reformed packing proposal.
- [Triage T2.c / `/cancel-all` standalone comment (#issuecomment-4329511044)](https://github.com/openabdev/openab/issues/580#issuecomment-4329511044) — Phase 1 timing rationale.
- [RFC #580 Tier 2 round-up + masami acceptance criteria](https://github.com/openabdev/openab/issues/580) — observability spec, Slack/Gateway integration, graceful shutdown, batch reaction UX, Scenario D smoke matrix.
- ADR: [Multi-Platform Adapter Architecture](./multi-platform-adapters.md) — defines the `SenderContext` record this ADR extends.
- ADR: [Custom Gateway for Webhook-Based Platform Integration](./custom-gateway.md) — establishes the ISO 8601 / RFC 3339 UTC timestamp convention this ADR extends to `<sender_context>` JSON.
- [Tokio `Mutex` documentation](https://docs.rs/tokio/latest/tokio/sync/struct.Mutex.html) — basis for the not-strictly-FIFO ordering claim.
- [Documenting Architecture Decisions — Michael Nygard (2011)](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions.html).

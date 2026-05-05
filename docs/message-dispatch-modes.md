# Message Dispatch Modes

OpenAB supports three message dispatch modes that control how incoming messages are batched before being sent to the AI agent as an ACP turn.

## Modes

### `per-message` (default)

Each message triggers its own ACP turn. This is the default behavior — simple, predictable, no batching. Existing deployments use this mode automatically without any configuration change.

**Pros:**
- Zero added latency — every message dispatches immediately
- Simplest mental model (1 message = 1 agent response)
- No configuration needed
- No risk of messages being grouped with unrelated context

**Cons:**
- Redundant context: if a user sends 3 messages quickly, the agent loads full context 3 times
- Higher token cost under burst traffic (N messages = N full context windows)
- Agent responds to each message independently — can't synthesize a coherent answer across rapid-fire messages

### `per-thread`

All messages in a thread share one buffer. Messages that arrive while the agent is processing are batched into a single ACP turn at the next turn boundary.

**Pros:**
- Lowest token cost — one context window serves N messages
- Agent sees the full picture and can synthesize a single coherent response
- Reduces "sorry I already answered that" noise from sequential turns

**Cons:**
- Silent-drop risk: if Alice and Bob both message, the agent may only address Alice
- Single response must cover all senders — harder for the agent to structure
- Slight latency for messages 2..N (they wait for the current turn to finish)

### `per-lane`

Each (thread, sender) pair gets its own buffer. Messages from the same sender batch together, but different senders get independent ACP turns.

**Pros:**
- No silent-drop risk — every sender gets their own dedicated turn and response
- Same-sender batching still reduces redundant context (rapid-fire messages consolidated)
- Best for multi-agent collaboration (each bot gets its own turn)

**Cons:**
- Higher token cost than `per-thread` (one turn per active sender per boundary)
- Turns serialize through the shared session — sender B waits for sender A's turn to complete
- More complex to reason about (ordering depends on arrival time)

## Comparison Table

| | `per-message` | `per-thread` | `per-lane` |
|---|---|---|---|
| Token cost | Highest (N turns) | Lowest (1 turn) | Medium (1 per sender) |
| Latency | Zero | Wait for turn boundary | Wait for turn boundary |
| Silent-drop risk | None | Yes (multi-sender) | None |
| Rapid-fire handling | Each separate | All batched | Same-sender batched |
| Best for | Single user, low traffic | Cost-sensitive, single-reply | Multi-user, multi-bot |
| Configuration | None (default) | Opt-in | Opt-in |

## Decision Guide

```
Is this a single-user bot (1 human per thread)?
  → per-message (default, simplest)

Multiple humans in the same thread?
  ├─ Is it OK if the agent addresses everyone in one reply?
  │    → per-thread (cheapest)
  └─ Each person needs their own response?
       → per-lane (safest)

Multi-agent collaboration (bot-to-bot)?
  → per-lane (each bot gets its own turn)
```

## Configuration

### config.toml

```toml
[discord]
message_processing_mode = "per-lane"   # "per-message" | "per-thread" | "per-lane"
max_buffered_messages = 10             # per-thread mpsc capacity (batched modes only)
max_batch_tokens = 24000               # soft token cap per ACP turn (batched modes only)
```

### Helm values

```yaml
agents:
  kiro:
    discord:
      messageProcessingMode: "per-lane"
      maxBufferedMessages: 10
      maxBatchTokens: 24000
```

The same fields are available under `slack:` and `gateway:` sections.

## How It Works — ASCII Diagrams

### per-message (default)

```
Time ──────────────────────────────────────────────────────►

Alice: "hi"          Alice: "also this"       Bob: "hey"
   │                      │                      │
   ▼                      ▼                      ▼
┌──────────┐         ┌──────────┐          ┌──────────┐
│ ACP Turn │         │ ACP Turn │          │ ACP Turn │
│ (1 msg)  │         │ (1 msg)  │          │ (1 msg)  │
└────┬─────┘         └────┬─────┘          └────┬─────┘
     ▼                    ▼                      ▼
  Response 1           Response 2             Response 3

Each message = its own turn. Simple. 3 messages → 3 responses.
```

### per-thread

```
Time ──────────────────────────────────────────────────────►

Alice: "hi"     Bob: "hey"   Alice: "also this"
   │               │              │
   ▼               │              │
┌──────────┐       │              │
│ ACP Turn │  (agent busy...)     │
│ (1 msg)  │       │              │
└────┬─────┘       ▼              ▼
     │         ┌────────────────────────┐
     ▼         │ Buffer (shared thread) │
  Response 1   │  → Bob: "hey"         │
               │  → Alice: "also this" │
               └───────────┬────────────┘
                           ▼  (turn boundary)
                    ┌──────────────┐
                    │  ACP Turn    │
                    │  (2 msgs     │
                    │   batched)   │
                    └──────┬───────┘
                           ▼
                      Response 2
                   (addresses both)

All senders share one buffer → one batched turn → one response.
```

### per-lane

```
Time ──────────────────────────────────────────────────────►

Alice: "hi"     Bob: "hey"   Alice: "also this"
   │               │              │
   ▼               │              │
┌──────────┐       │              │
│ ACP Turn │  (agent busy...)     │
│ (Alice)  │       │              │
└────┬─────┘       ▼              ▼
     │      ┌─────────────┐  ┌──────────────────┐
     ▼      │ Bob's lane  │  │ Alice's lane     │
Response 1  │ → "hey"     │  │ → "also this"    │
            └──────┬──────┘  └────────┬─────────┘
                   ▼                  │
            ┌──────────────┐          │  (waits for Bob's turn)
            │  ACP Turn    │          │
            │  (Bob, 1msg) │          │
            └──────┬───────┘          │
                   ▼                  ▼
              Response 2       ┌──────────────┐
              (for Bob)        │  ACP Turn    │
                               │  (Alice,1msg)│
                               └──────┬───────┘
                                      ▼
                                 Response 3
                                 (for Alice)

Each sender gets their own lane → own turn → own response. No silent drop.
```

### Batching internals (consumer loop)

```
                    ┌─────────────────────────────────────┐
                    │         Dispatcher (per thread)      │
                    │                                     │
  submit(msg) ─────►  mpsc channel (cap = max_buffered)  │
                    │         │                           │
                    │         ▼                           │
                    │  ┌─────────────────────────┐       │
                    │  │    consumer_loop         │       │
                    │  │                         │       │
                    │  │  1. Block on first msg  │       │  ← I1: zero latency
                    │  │     (or idle timeout)   │       │
                    │  │                         │       │
                    │  │  2. Greedy drain:       │       │
                    │  │     while try_recv()    │       │
                    │  │       && count < cap    │       │
                    │  │       && tokens < max   │       │
                    │  │                         │       │
                    │  │  3. Pack batch:         │       │
                    │  │     [sender_ctx + msg]  │       │
                    │  │     [sender_ctx + msg]  │       │
                    │  │     ...                 │       │
                    │  │                         │       │
                    │  │  4. stream_prompt_blocks│       │  ← I2: one turn at a time
                    │  │     (shared session)    │       │
                    │  │                         │       │
                    │  │  5. Loop back to 1      │       │
                    │  └─────────────────────────┘       │
                    └─────────────────────────────────────┘
```

## Defaults

| Parameter | Default | Notes |
|-----------|---------|-------|
| `message_processing_mode` | `per-message` | Backward compatible, no batching |
| `max_buffered_messages` | 10 | Only applies to `per-thread` / `per-lane` |
| `max_batch_tokens` | 24000 | Rough estimate (~4 chars/token) |

# Testing multi-bubble delivery, offline

Everything in this directory runs the broker's delivery paths **without an LLM
key, a chat platform account, or a network**. Two fakes stand in for the two ends:

```
scripts/fake-gateway.py     a chat platform you can read
        ▲  ws://127.0.0.1:8765
        │  openab.gateway.reply.v1  ← one frame per delivered message
    ┌───┴────┐
    │ openab │  ← the code under test
    └───┬────┘
        │  ACP stdio JSON-RPC
        ▼
scripts/fake-acp-agent.py   an agent that replays a scripted turn
```

The gateway prints each delivered message as a numbered bubble with the gap
since the previous one, so "did that turn produce one message or three, and how
far apart" is something you *see*, not something you infer from a log.

## Run it

Two terminals.

**Terminal 1 — the fake platform:**

```sh
uv run scripts/fake-gateway.py
```

**Terminal 2 — the broker** (from the repo root):

```sh
cargo run -- run -c scripts/bubble-test/structured.toml
```

Then type in terminal 1 and press enter. Anything you type is sent as a user
message; `/proactive <text>` sends an unsolicited event.

> Running openab in Docker? Start the gateway with `FAKE_GATEWAY_HOST=0.0.0.0`
> and change the config's `url` to `ws://host.docker.internal:8765`.

## What to check

Switch scenarios by editing `SCENARIO` in the config's `[agent] env`.

### `structured.toml` — Phase 1 + 2

| SCENARIO | Expected |
|---|---|
| `envelope` | **3 separate bubbles**, ~400ms apart (`bubble_delay_ms`) |
| `multiline` | **1 bubble, 3 lines** — a newline is not a message boundary |
| `silent` | **nothing at all** |
| `prose` | one message, verbatim — the model forgot the envelope, the user still gets a reply |
| `broken` | `sorry, one sec` and **no JSON** — the truncated envelope is stripped |
| `toolong` | the `parse_error_text` line — over `max_bubbles`, and the body was nothing but the envelope, so stripping it leaves nothing to say |

The `broken` case is the important one. Confirm the log says
`structured turn did not parse … policy=fallback_text`, and that **no `{` reaches
the gateway**.

### `sequential.toml` — Phase 4

| SCENARIO | Expected |
|---|---|
| `sequential` | 3 bubbles, back to back |
| `seqslow` | 3 bubbles **~2s apart** — the pause is the *agent's*, and `bubble_delay_ms` is 0, so this proves each bubble was sent when it was decided rather than batched at the end |
| `seqhalf` | 2 bubbles, then the turn fails — the user keeps what arrived |

### `triage.toml` — Phase 3

Send a normal message, then `/proactive flight delayed`.

- The **normal message must be delivered** — quiet hours never apply to
  something a human sent. This is the property that matters most.
- The **proactive event must be suppressed**, and the log must name why:
  `decision="suppressed" reason="quiet_hours"`.

Then exercise the other rules:

- `/proactive one` twice with the same text → the second is `cooldown`
  (change `quiet_hours` to a window you are *not* in first, e.g. `"03:00-03:01"`)
- `/id evt_42 hello` twice → the second is `duplicate` (a webhook retry)
- four events inside a minute → the fourth is `daily_cap` (`daily_cap = 3`)

### `text.toml` — regression baseline

Nothing configured. One ordinary message. If this changes, something that was
supposed to be opt-in is not.

## The pairing that must not ship

With a **real** agent, the broker's `[delivery] mode` and the agent's
`turn_envelope` / `OPENAB_AGENT_SEQUENTIAL_BUBBLES` must agree. Mismatches range
from harmless to "the user sees raw JSON" or "the user sees nothing" — the full
table is in [docs/native-agent.md](../../docs/native-agent.md#pairing-with-the-broker).

These fakes deliberately let you produce the bad combinations, so you can see
what each one looks like before it happens in production.

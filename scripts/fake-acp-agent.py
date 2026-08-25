#!/usr/bin/env python3
"""
Fake ACP agent — replays a scripted turn so the broker's delivery paths can be
exercised without an LLM key, a network, or a real coding CLI.

Speaks just enough ACP over stdio JSON-RPC for `openab` to drive it:
`initialize`, `session/new`, `session/prompt`, `session/cancel`.

Pick what the turn does with SCENARIO:

    envelope     three bubbles in one openab.turn.v1 envelope   [delivery] mode = "structured"
    multiline    ONE bubble containing three lines               "
    silent       an envelope that asks to send nothing           "
    toolong      five bubbles — over the default max_bubbles     "
    broken       a truncated envelope (leak check)               "
    prose        no envelope at all, just text                   "
    sequential   three openab_message events, emitted live      [delivery] mode = "sequential"
    seqslow      same, with a 2s pause between each              "
    seqhalf      two events then an error mid-turn               "
    text         plain text                                      [delivery] mode = "text"

Usage — normally not run by hand; point config.toml at it:

    [agent]
    command = "python3"
    args = ["/abs/path/to/scripts/fake-acp-agent.py"]
    env = { SCENARIO = "envelope" }

Every decision it makes is logged to stderr, which `openab` surfaces in its own
output, so you can see what the agent claimed to send versus what arrived.
"""
import json
import os
import sys
import time

SCHEMA = "openab.turn.v1"
SCENARIO = os.environ.get("SCENARIO", "envelope")


def log(msg):
    print(f"[fake-agent] {msg}", file=sys.stderr, flush=True)


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def notify(session_id, update):
    send({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {"sessionId": session_id, "update": update},
    })


def chunk(session_id, text):
    """The standard event: accumulates into the broker's turn buffer."""
    notify(session_id, {
        "sessionUpdate": "agent_message_chunk",
        "content": {"type": "text", "text": text},
    })


def bubble(session_id, index, text):
    """The Phase 4 extension: delivered by the broker on arrival."""
    notify(session_id, {
        "sessionUpdate": "openab_message",
        "id": f"bubble_{index}",
        "content": {"type": "text", "text": text},
    })


def envelope(messages, next_type="stop"):
    return json.dumps({
        "schema": SCHEMA,
        "messages": [
            {"id": f"bubble_{i + 1}", "text": t} for i, t in enumerate(messages)
        ],
        "next": {"type": next_type},
    })


def run_turn(session_id):
    """Play the configured scenario. Returns the JSON-RPC result."""
    log(f"turn start, scenario={SCENARIO}")

    if SCENARIO == "envelope":
        chunk(session_id, envelope(["on it", "your flight moved to 8pm", "gate B12"]))

    elif SCENARIO == "multiline":
        # The load-bearing check: this must arrive as ONE message, not three.
        chunk(session_id, envelope(["alpha\nbeta\ngamma"]))

    elif SCENARIO == "silent":
        chunk(session_id, envelope([], next_type="silent"))

    elif SCENARIO == "toolong":
        chunk(session_id, envelope(["one", "two", "three", "four", "five"]))

    elif SCENARIO == "broken":
        # Truncated mid-object. The user must NOT see this.
        chunk(session_id, 'sorry, one sec\n{"schema":"openab.turn.v1","messages":[{"id":"b1","te')

    elif SCENARIO == "prose":
        chunk(session_id, "hey what's up")

    elif SCENARIO in ("sequential", "seqslow", "seqhalf"):
        gap = 2.0 if SCENARIO == "seqslow" else 0.0
        parts = ["on it", "checking your mail now", "your flight moved to 8pm\ngate B12"]
        if SCENARIO == "seqhalf":
            parts = parts[:2]
        for i, text in enumerate(parts, start=1):
            if gap and i > 1:
                time.sleep(gap)
            log(f"emitting bubble_{i}: {text!r}")
            bubble(session_id, i, text)
        if SCENARIO == "seqhalf":
            log("failing mid-turn on purpose")
            return {"error": {"code": -32000, "message": "simulated agent failure"}}

    elif SCENARIO == "text":
        chunk(session_id, "plain text reply, one message")

    else:
        log(f"unknown SCENARIO {SCENARIO!r}; falling back to prose")
        chunk(session_id, f"unknown scenario {SCENARIO}")

    return {"result": {"stopReason": "end_turn"}}


def main():
    log(f"started, scenario={SCENARIO}")
    session_counter = 0
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            log(f"ignoring non-JSON line: {line[:80]}")
            continue

        method = req.get("method")
        req_id = req.get("id")

        if method == "initialize":
            send({
                "jsonrpc": "2.0",
                "id": req_id,
                "result": {
                    "protocolVersion": 1,
                    "agentInfo": {"name": "fake-acp-agent", "version": "1"},
                    "agentCapabilities": {"streaming": True},
                },
            })

        elif method == "session/new":
            session_counter += 1
            session_id = f"fake-session-{session_counter}"
            send({"jsonrpc": "2.0", "id": req_id, "result": {"sessionId": session_id}})

        elif method == "session/prompt":
            session_id = (req.get("params") or {}).get("sessionId", "fake-session-1")
            prompt = json.dumps((req.get("params") or {}).get("prompt", ""))[:120]
            log(f"prompt received: {prompt}")
            outcome = run_turn(session_id)
            send({"jsonrpc": "2.0", "id": req_id, **outcome})

        elif method == "session/cancel":
            log("cancel received")
            if req_id is not None:
                send({"jsonrpc": "2.0", "id": req_id, "result": {}})

        elif req_id is not None:
            send({
                "jsonrpc": "2.0",
                "id": req_id,
                "error": {"code": -32601, "message": f"method not found: {method}"},
            })


if __name__ == "__main__":
    try:
        main()
    except (BrokenPipeError, KeyboardInterrupt):
        pass

#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["websockets>=12"]
# ///
"""
Fake Custom Gateway — a chat platform you can read.

`openab` connects here as a WebSocket client and delivers every outbound message
as an `openab.gateway.reply.v1` frame. This prints each one as a numbered,
timestamped bubble, so "did that turn produce one message or three, and how far
apart" is something you can see rather than infer.

No platform account, no tokens, no LLM key. Pair it with
`scripts/fake-acp-agent.py` and the whole broker delivery path runs offline.

Usage:

    uv run scripts/fake-gateway.py                 # listens on ws://127.0.0.1:8765

Then in the same terminal, type to send a message to the agent:

    hello                       send a normal user message
    /proactive flight delayed   send an UNSOLICITED event (exercises [triage])
    /id evt_42 <text>           send with a specific event_id (retry / dedupe test)
    /quit                       stop

Point openab at it:

    [gateway]
    url = "ws://127.0.0.1:8765"
    platform = "telegram"
    allow_all_users = true
    streaming = false
"""
import asyncio
import json
import os
import sys
import time
import uuid

import websockets

# Loopback by default. Set FAKE_GATEWAY_HOST=0.0.0.0 when openab runs in a
# container and has to reach this from outside (then point its `url` at
# ws://host.docker.internal:8765).
HOST = os.environ.get("FAKE_GATEWAY_HOST", "127.0.0.1")
PORT = int(os.environ.get("FAKE_GATEWAY_PORT", "8765"))

RESET, DIM, BOLD = "\033[0m", "\033[2m", "\033[1m"
GREEN, YELLOW, CYAN, RED = "\033[32m", "\033[33m", "\033[36m", "\033[31m"

_clients: set = set()
_turn_start: float | None = None
_bubble_count = 0


def stamp() -> str:
    return time.strftime("%H:%M:%S")


def note(msg: str, colour: str = DIM) -> None:
    print(f"{colour}[{stamp()}] {msg}{RESET}", flush=True)


def show_reply(reply: dict) -> None:
    """Print one delivered bubble, with the gap since the previous one."""
    global _bubble_count, _turn_start
    _bubble_count += 1
    now = time.monotonic()
    gap = ""
    if _turn_start is not None:
        gap = f"  {DIM}(+{now - _turn_start:.2f}s){RESET}"
    _turn_start = now

    text = (reply.get("content") or {}).get("text", "")
    quoted = reply.get("quote_message_id")
    header = f"{BOLD}{GREEN}◀ bubble #{_bubble_count}{RESET}"
    if quoted:
        header += f" {CYAN}(reply to {quoted}){RESET}"
    print(f"{header}{gap}", flush=True)
    for line in text.split("\n"):
        print(f"   │ {line}", flush=True)
    # A multi-line bubble printed under ONE header is the thing to look for:
    # three headers would mean the newline became a message boundary.
    if "\n" in text:
        print(f"   {DIM}└─ one message, {len(text.splitlines())} lines{RESET}", flush=True)


def build_event(text: str, proactive: bool = False, event_id: str | None = None) -> str:
    return json.dumps({
        "schema": "openab.gateway.event.v1",
        "event_id": event_id or f"evt-{uuid.uuid4().hex[:8]}",
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "platform": "telegram",
        "channel": {"id": "test-channel", "type": "dm"},
        "sender": {
            "id": "test-user",
            "name": "tester",
            "display_name": "Tester",
            "is_bot": False,
        },
        "content": {"type": "text", "text": text},
        "mentions": [],
        "message_id": f"msg-{uuid.uuid4().hex[:8]}",
        "proactive": proactive,
    })


async def handler(ws):
    _clients.add(ws)
    note("openab connected", GREEN)
    try:
        async for raw in ws:
            try:
                msg = json.loads(raw)
            except json.JSONDecodeError:
                note(f"non-JSON frame: {raw[:80]}", RED)
                continue

            schema = msg.get("schema", "")
            if schema == "openab.gateway.reply.v1":
                command = msg.get("command")
                if command:
                    # edit_message / add_reaction / … — not a delivered bubble.
                    note(f"command: {command}", DIM)
                else:
                    show_reply(msg)
                # Ack when asked. Structured/sequential mode turns this on so
                # bubbles cannot be reordered in flight.
                if msg.get("request_id"):
                    await ws.send(json.dumps({
                        "schema": "openab.gateway.response.v1",
                        "request_id": msg["request_id"],
                        "success": True,
                        "message_id": f"gw-{uuid.uuid4().hex[:6]}",
                        "thread_id": None,
                        "error": None,
                    }))
            else:
                note(f"frame: {schema or '(no schema)'}", DIM)
    except websockets.exceptions.ConnectionClosed:
        pass
    finally:
        _clients.discard(ws)
        note("openab disconnected", YELLOW)


async def broadcast(payload: str) -> None:
    global _turn_start, _bubble_count
    if not _clients:
        note("no openab connected yet", RED)
        return
    _turn_start = time.monotonic()
    _bubble_count = 0
    for ws in list(_clients):
        await ws.send(payload)


async def console() -> None:
    loop = asyncio.get_running_loop()
    while True:
        line = await loop.run_in_executor(None, sys.stdin.readline)
        if not line:
            break
        line = line.strip()
        if not line:
            continue
        if line == "/quit":
            break
        if line.startswith("/proactive "):
            text = line[len("/proactive "):]
            note(f"▶ proactive event: {text!r}", YELLOW)
            await broadcast(build_event(text, proactive=True))
        elif line.startswith("/id "):
            _, event_id, *rest = line.split(" ", 2)
            text = rest[0] if rest else "retry"
            note(f"▶ event_id={event_id}: {text!r}", YELLOW)
            await broadcast(build_event(text, proactive=True, event_id=event_id))
        else:
            note(f"▶ user: {line!r}", CYAN)
            await broadcast(build_event(line))


async def main() -> None:
    async with websockets.serve(handler, HOST, PORT):
        note(f"listening on ws://{HOST}:{PORT} — waiting for openab", BOLD)
        note("type a message and press enter; /proactive <text> for an unsolicited event", DIM)
        await console()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass

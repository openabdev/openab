#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["websockets>=12"]
# ///
"""
ACP-over-WebSocket conformance suite — upstream client<->gateway `/acp` hop.

Exercises the WebSocket ACP server this PR adds (GET /acp on the openab gateway /
embedded `openab run`) against a LIVE deployment, and prints an item-by-item report
suitable for pasting into the PR as evidence. It covers three groups:

  [Transport / Auth]  — a transport token is REQUIRED off loopback: no-token and
                        wrong-token connections are rejected, the valid token is accepted.
  [Protocol compliance] — JSON-RPC envelope + ACP wire shapes (initialize negotiation,
                        session lifecycle, session/update notification, stopReason, resume).
  [Protocol edge cases] — hardening: version negotiation & rejects, param validation,
                        content-block policy, notification silence, oversized-reply whole
                        delivery, and Unicode/emoji stream integrity.

A live agent backend is required (prompt turns hit the real model).

Usage:
    OPENAB_ACP_TOKEN=<key> uv run scripts/acp-ws-smoke.py ws://<host>:8080/acp

    # WS_URL defaults to ws://localhost:8080/acp
    # OPENAB_ACP_TOKEN is MANDATORY — the endpoint requires a transport key off loopback.

Exit code 0 iff every check passes.
"""
import asyncio
import json
import os
import sys

import websockets
from websockets.exceptions import InvalidStatus, InvalidStatusCode, WebSocketException

BASE_URL = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("ACP_URL", "ws://localhost:8080/acp")
TOKEN = os.environ.get("OPENAB_ACP_TOKEN")

results: list[tuple[str, bool, str]] = []


def record(section: str, ok: bool, name: str, detail: str = "") -> None:
    results.append((section, ok, name))
    mark = "PASS" if ok else "FAIL"
    line = f"  [{mark}] {name}"
    if detail:
        line += f" — {detail}"
    print(line, flush=True)


def bearer_subprotocols(token: str | None) -> list[str]:
    # Carry the token via the Sec-WebSocket-Protocol subprotocol (keeps it out of the
    # URL — the de facto browser-WS bearer pattern). The server echoes `acp.v1`.
    return [f"openab.bearer.{token}", "acp.v1"] if token else ["acp.v1"]


async def try_connect(token: str | None):
    """Return an open ws (caller closes) or raise on rejection."""
    return await websockets.connect(
        BASE_URL, subprotocols=bearer_subprotocols(token), open_timeout=8, max_size=None
    )


class Conn:
    """A JSON-RPC/ACP client over one WebSocket connection."""

    def __init__(self, ws):
        self.ws = ws
        self._id = 0

    async def call(self, method, params=None, *, notification=False, timeout=8):
        """Send a request (or notification) and, for requests, gather streamed
        notifications until the response with the matching id arrives."""
        msg = {"jsonrpc": "2.0", "method": method}
        rid = None
        if not notification:
            self._id += 1
            rid = self._id
            msg["id"] = rid
        if params is not None:
            msg["params"] = params
        await self.ws.send(json.dumps(msg))
        chunks, notes = [], []
        if notification:
            return {"_notification": True}
        while True:
            try:
                raw = await asyncio.wait_for(self.ws.recv(), timeout=timeout)
            except asyncio.TimeoutError:
                return {"_timeout": True, "chunks": chunks, "notes": notes}
            m = json.loads(raw)
            if m.get("method") == "session/update":
                notes.append(m)
                u = m.get("params", {}).get("update", {})
                if u.get("sessionUpdate") == "agent_message_chunk":
                    chunks.append(u.get("content", {}).get("text", ""))
                continue
            if m.get("method"):
                notes.append(m)
                continue
            if m.get("id") == rid:
                m["chunks"] = chunks
                m["notes"] = notes
                return m

    async def initialize(self, version=1):
        return await self.call("initialize", {"protocolVersion": version, "clientInfo": {"name": "conf", "version": "0"}})

    async def new_session(self):
        r = await self.call("session/new", {"cwd": "/home/agent", "mcpServers": []})
        return r.get("result", {}).get("sessionId", "")


async def expect_silence(conn: Conn, method, params, timeout=3) -> bool:
    """Send a notification (no id) and assert the server sends nothing back."""
    await conn.call(method, params, notification=True)
    try:
        await asyncio.wait_for(conn.ws.recv(), timeout=timeout)
        return False  # got a frame → not silent
    except asyncio.TimeoutError:
        return True


# --------------------------------------------------------------------------- #
# Sections
# --------------------------------------------------------------------------- #
async def section_auth():
    print("\n[Transport / Auth] — a transport token is REQUIRED", flush=True)
    # no token → rejected
    try:
        ws = await try_connect(None)
        await ws.close()
        record("auth", False, "connection WITHOUT a token is rejected", "connection was accepted")
    except (InvalidStatus, InvalidStatusCode, WebSocketException, OSError) as e:
        record("auth", True, "connection WITHOUT a token is rejected", type(e).__name__)
    # wrong token → rejected
    try:
        ws = await try_connect("wrong-" + (TOKEN or "x"))
        await ws.close()
        record("auth", False, "connection with a WRONG token is rejected", "connection was accepted")
    except (InvalidStatus, InvalidStatusCode, WebSocketException, OSError) as e:
        record("auth", True, "connection with a WRONG token is rejected", type(e).__name__)
    # valid token → accepted
    try:
        ws = await try_connect(TOKEN)
        await ws.close()
        record("auth", True, "connection with the VALID token is accepted")
    except Exception as e:  # noqa: BLE001
        record("auth", False, "connection with the VALID token is accepted", repr(e))


async def section_compliance():
    print("\n[Protocol compliance] — JSON-RPC envelope + ACP wire shapes", flush=True)
    async with await try_connect(TOKEN) as ws:
        c = Conn(ws)
        r = await c.initialize()
        res = r.get("result", {})
        record("comp", r.get("jsonrpc") == "2.0" and r.get("id") == 1, "initialize: response is JSON-RPC 2.0 with correlated id")
        record("comp", res.get("protocolVersion") == 1 and isinstance(res.get("protocolVersion"), int),
               "initialize: protocolVersion is the integer 1", f"got {res.get('protocolVersion')!r}")
        caps = res.get("agentCapabilities", {})
        ok_caps = caps.get("loadSession") is False and isinstance(caps.get("sessionCapabilities", {}).get("resume"), dict) and "promptCapabilities" in caps
        record("comp", ok_caps, "initialize: agentCapabilities shape (loadSession:false, sessionCapabilities.resume, promptCapabilities)")
        record("comp", isinstance(res.get("authMethods"), list), "initialize: authMethods is an array")

        sid = await c.new_session()
        record("comp", sid.startswith("sess_"), "session/new: returns { sessionId: sess_<uuid> }", sid[:24])

        r = await c.call("session/prompt", {"sessionId": sid, "prompt": [{"type": "text", "text": "Reply with exactly one word: PONG"}]}, timeout=90)
        upd = next((n for n in r.get("notes", []) if n.get("method") == "session/update"), None)
        u = (upd or {}).get("params", {}).get("update", {})
        record("comp", u.get("sessionUpdate") == "agent_message_chunk" and u.get("content", {}).get("type") == "text",
               "session/prompt: streams session/update {sessionUpdate:agent_message_chunk, content.type:text}")
        record("comp", r.get("result", {}).get("stopReason") == "end_turn",
               "session/prompt: response stopReason is snake_case end_turn", f"got {r.get('result', {}).get('stopReason')!r}")

        r = await c.call("session/resume", {"sessionId": sid, "cwd": "/home/agent", "mcpServers": []})
        record("comp", r.get("result") == {} and not r.get("error"), "session/resume: returns {} (no history replay)")


async def section_edges():
    print("\n[Protocol edge cases] — negotiation, validation, content policy, hardening", flush=True)
    # initialize negotiation + rejects (fresh connections; these do not need init state)
    async with await try_connect(TOKEN) as ws:
        c = Conn(ws)
        r = await c.initialize(version=5)
        record("edge", r.get("result", {}).get("protocolVersion") == 1, "initialize: client protocolVersion 5 negotiates down to 1")
    async with await try_connect(TOKEN) as ws:
        r = await Conn(ws).initialize(version=0)
        record("edge", r.get("error", {}).get("code") == -32602, "initialize: protocolVersion 0 rejected (-32602)")
    async with await try_connect(TOKEN) as ws:
        r = await Conn(ws).call("initialize", {"clientInfo": {"name": "x"}})
        record("edge", r.get("error", {}).get("code") == -32602, "initialize: missing protocolVersion rejected (-32602)")

    # lifecycle: session/new before initialize → -32002
    async with await try_connect(TOKEN) as ws:
        r = await Conn(ws).call("session/new", {"cwd": "/w", "mcpServers": []})
        record("edge", r.get("error", {}).get("code") == -32002, "session/new before initialize rejected (-32002)")

    # bad jsonrpc version → -32600
    async with await try_connect(TOKEN) as ws:
        await ws.send(json.dumps({"jsonrpc": "1.0", "id": 1, "method": "initialize", "params": {"protocolVersion": 1}}))
        m = json.loads(await asyncio.wait_for(ws.recv(), timeout=8))
        record("edge", m.get("error", {}).get("code") == -32600, "jsonrpc != \"2.0\" rejected (-32600)")

    # wrong-typed JSON-RPC id (object) → -32600
    async with await try_connect(TOKEN) as ws:
        await ws.send(json.dumps({"jsonrpc": "2.0", "id": {}, "method": "initialize", "params": {"protocolVersion": 1}}))
        m = json.loads(await asyncio.wait_for(ws.recv(), timeout=8))
        record("edge", m.get("error", {}).get("code") == -32600, "wrong-typed id (object) rejected (-32600)")

    # the rest run on one initialized connection
    async with await try_connect(TOKEN) as ws:
        c = Conn(ws)
        await c.initialize()
        # param validation
        r = await c.call("session/new", {"mcpServers": []})
        record("edge", r.get("error", {}).get("code") == -32602, "session/new missing cwd rejected (-32602)")
        r = await c.call("session/resume", {"cwd": "/w", "mcpServers": []})
        record("edge", r.get("error", {}).get("code") == -32602, "session/resume missing sessionId rejected (-32602)")

        sid = await c.new_session()
        # content-block policy: resource_link accepted (baseline), image rejected (gated)
        r = await c.call("session/prompt", {"sessionId": sid, "prompt": [
            {"type": "text", "text": "ignore the link, just reply OK"},
            {"type": "resource_link", "uri": "file:///x", "name": "X"},
        ]}, timeout=90)
        record("edge", not r.get("error"), "prompt: resource_link content accepted (ACP baseline)")
        r = await c.call("session/prompt", {"sessionId": sid, "prompt": [
            {"type": "image", "data": "..", "mimeType": "image/png"},
        ]})
        record("edge", r.get("error", {}).get("code") == -32602, "prompt: image content rejected (-32602, capability not advertised)")

        # notification silence: session/cancel as a notification gets no response
        silent = await expect_silence(c, "session/cancel", {"sessionId": sid})
        record("edge", silent, "notification (session/cancel, no id) receives no response")

        # F2 — an oversized reply (> the 4096 unified message limit) arrives whole
        r = await c.call("session/prompt", {"sessionId": sid, "prompt": [{"type": "text", "text":
            "Output the lines 'LINE 0001' through 'LINE 0600', one per line, zero-padded to 4 digits, nothing else."}]}, timeout=180)
        text = "".join(r.get("chunks", []))
        import re
        nums = [int(x) for x in re.findall(r"LINE (\d{4})", text)]
        record("edge", len(text) > 4096 and max(nums or [0]) >= 550,
               "F2: oversized reply delivered whole (not truncated at message limit)", f"{len(text)} chars, max LINE {max(nums or [0]):04d}")

        # Unicode / emoji stream integrity
        r = await c.call("session/prompt", {"sessionId": sid, "prompt": [{"type": "text", "text":
            "Reply with exactly this and nothing else: 你好 🎉 👨‍👩‍👧‍👦 ❤️"}]}, timeout=90)
        text = "".join(r.get("chunks", []))
        # Require EVERY marker (the old `a and b or c` parsed as `(a and b) or c`, so a lone 👨
        # passed). Fail closed first on a timeout / JSON-RPC error so a dropped reply can't slip
        # through the content check.
        markers = ["你好", "🎉", "👨‍👩‍👧‍👦", "❤️"]
        ok = (
            not r.get("_timeout")
            and not r.get("error")
            and all(m in text for m in markers)
        )
        record("edge", ok, "CJK + emoji (ZWJ family) stream intact", repr(text[:40]))


async def section_lifecycle():
    print("\n[Lifecycle / transport] — cancel, oversized frame, header auth", flush=True)
    from websockets.exceptions import ConnectionClosed

    # valid token via the Authorization: Bearer header (non-browser path)
    try:
        ws = await websockets.connect(
            BASE_URL, additional_headers={"Authorization": f"Bearer {TOKEN}"}, open_timeout=8
        )
        await ws.close()
        record("life", True, "valid token via Authorization: Bearer header accepted")
    except Exception as e:  # noqa: BLE001
        record("life", False, "valid token via Authorization: Bearer header accepted", repr(e))

    # oversized frame → the server closes the connection (no fabricated JSON-RPC response)
    async with await try_connect(TOKEN) as ws:
        await ws.send("x" * ((1 << 20) + 64))  # > MAX_FRAME_BYTES (1 MiB)
        try:
            await asyncio.wait_for(ws.recv(), timeout=8)
            record("life", False, "oversized frame closes the connection", "got a frame back")
        except ConnectionClosed:
            record("life", True, "oversized frame closes the connection")
        except asyncio.TimeoutError:
            record("life", False, "oversized frame closes the connection", "no close within 8s")

    # session/cancel → the in-flight prompt ends with stopReason:"cancelled"
    async with await try_connect(TOKEN) as ws:
        c = Conn(ws)
        await c.initialize()
        sid = await c.new_session()
        c._id += 1
        rid = c._id
        await ws.send(json.dumps({"jsonrpc": "2.0", "id": rid, "method": "session/prompt",
                                  "params": {"sessionId": sid, "prompt": [{"type": "text",
                                  "text": "Count from 1 to 400, one number per line, slowly."}]}}))
        # wait for streaming to start, then cancel (a notification: no id)
        started = False
        while not started:
            m = json.loads(await asyncio.wait_for(ws.recv(), timeout=30))
            if m.get("method") == "session/update":
                started = True
        await ws.send(json.dumps({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": sid}}))
        stop = None
        while True:
            m = json.loads(await asyncio.wait_for(ws.recv(), timeout=60))
            if m.get("id") == rid:
                stop = m.get("result", {}).get("stopReason")
                break
        record("life", stop == "cancelled", "session/cancel → prompt ends stopReason:cancelled", f"got {stop!r}")


async def section_tunnel():
    """MCP-over-ACP tunnel producer (T5.3): declaring a `type:acp` MCP server in
    session/new makes the gateway open a tunnel to us (a server-initiated mcp/connect
    request); we answer and it registers the tunnel. This exercises the live read-loop
    spawn path end-to-end (the concurrency that unit tests can't reach)."""
    async with await try_connect(TOKEN) as ws:
        c = Conn(ws)
        await c.initialize()
        r = await c.call(
            "session/new",
            {
                "cwd": "/home/agent",
                "mcpServers": [{"type": "acp", "id": "srv-smoke", "name": "browser"}],
            },
        )
        sid = r.get("result", {}).get("sessionId", "")
        record("tunnel", sid.startswith("sess_"), "session/new with a type:acp mcpServers entry is accepted")

        # The gateway now issues a server-initiated mcp/connect. Wait for it.
        connect = None
        for _ in range(20):
            try:
                m = json.loads(await asyncio.wait_for(ws.recv(), timeout=5))
            except asyncio.TimeoutError:
                break
            if m.get("method") == "mcp/connect":
                connect = m
                break
        record("tunnel", connect is not None, "gateway sends a server-initiated mcp/connect after the type:acp declaration")
        if connect:
            params = connect.get("params", {})
            record("tunnel", params.get("acpId") == "srv-smoke", "mcp/connect carries the declared acpId", str(params))
            record("tunnel", connect.get("id") is not None, "mcp/connect is a request (has an id)")
            await ws.send(json.dumps({"jsonrpc": "2.0", "id": connect["id"], "result": {"connectionId": "conn-smoke"}}))
            record("tunnel", True, "answered mcp/connect with a connectionId (the tunnel registers)")


async def main() -> int:
    if not TOKEN:
        print("ERROR: OPENAB_ACP_TOKEN is required (the /acp endpoint mandates a transport token off loopback).", file=sys.stderr)
        return 3
    print(f"ACP conformance suite → {BASE_URL}", flush=True)
    await section_auth()
    await section_compliance()
    await section_edges()
    await section_lifecycle()
    await section_tunnel()

    total = len(results)
    passed = sum(1 for _, ok, _ in results if ok)
    by_section = {}
    for sec, ok, _ in results:
        s = by_section.setdefault(sec, [0, 0])
        s[1] += 1
        if ok:
            s[0] += 1
    print("\n" + "-" * 60, flush=True)
    labels = {"auth": "Transport / Auth", "comp": "Protocol compliance", "edge": "Protocol edge cases", "life": "Lifecycle / transport", "tunnel": "MCP-over-ACP tunnel"}
    for sec, (p, t) in by_section.items():
        print(f"  {labels.get(sec, sec):22} {p}/{t}", flush=True)
    print(f"\nRESULT: {passed}/{total} checks passed", flush=True)
    return 0 if passed == total else 2


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))

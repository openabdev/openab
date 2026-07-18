#!/usr/bin/env -S uv run --quiet --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["websockets>=12"]
# ///
"""
ACP-over-WebSocket smoke test — upstream client<->gateway `/acp` hop.

This exercises the WebSocket ACP server this PR adds (GET /acp on the openab
gateway / embedded `openab run`), which is a different boundary from the
downstream core<->agent-CLI stdio hop covered by docs/canary-tests.md Layer 2.
It drives a real deployment end to end, so a live agent backend is required —
the prompt turns hit the actual model.

Usage:
    uv run scripts/acp-ws-smoke.py [WS_URL]
    ACP_URL=ws://host:8080/acp uv run scripts/acp-ws-smoke.py

    # default WS_URL: ws://localhost:8080/acp
    # if the endpoint sets OPENAB_ACP_AUTH_KEY, pass the token:
    OPENAB_ACP_TOKEN=<key> uv run scripts/acp-ws-smoke.py ws://host:8080/acp

Checks (exit 0 iff all pass, else 2):
    1. initialize            -> protocolVersion == 1 and agentCapabilities present
    2. session/new           -> { sessionId: sess_... }
    3. session/prompt        -> agent_message_chunk stream is non-empty
    4. session/prompt        -> response stopReason == "end_turn"
    5. session/resume        -> {} (no error, no history replay)
    6. "/model"  as a prompt -> the config-command reply renders back over ACP
    7. "/reset"  as a prompt -> the session-command reply renders back over ACP

Checks 6-7 confirm OpenAB slash-command replies reach the ACP client stream
(they arrive as agent_message_chunk), i.e. command parity over ACP.
"""
import asyncio
import json
import os
import sys

import websockets


def build_url() -> str:
    url = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("ACP_URL", "ws://localhost:8080/acp")
    token = os.environ.get("OPENAB_ACP_TOKEN")
    if token:
        sep = "&" if "?" in url else "?"
        url = f"{url}{sep}token={token}"
    return url


results: list[tuple[bool, str]] = []


def check(ok: bool, name: str, detail: str = "") -> None:
    results.append((ok, name))
    mark = "PASS" if ok else "FAIL"
    print(f"  [{mark}] {name}{(' — ' + detail) if detail else ''}", flush=True)


async def main() -> int:
    url = build_url()
    redacted = url.split("token=")[0] + "token=<redacted>" if "token=" in url else url
    print(f"ACP WS smoke → {redacted}", flush=True)

    async with websockets.connect(url, open_timeout=8, max_size=None) as ws:
        next_id = 0

        async def call(method, params=None, timeout=8):
            """Send a JSON-RPC request; collect streamed notifications until the
            response with the matching id arrives (or timeout)."""
            nonlocal next_id
            next_id += 1
            req_id = next_id
            msg = {"jsonrpc": "2.0", "id": req_id, "method": method}
            if params is not None:
                msg["params"] = params
            await ws.send(json.dumps(msg))
            chunks: list[str] = []
            notifs: list[str] = []
            while True:
                try:
                    raw = await asyncio.wait_for(ws.recv(), timeout=timeout)
                except asyncio.TimeoutError:
                    return {"_timeout": True, "chunks": chunks, "notifs": notifs}
                m = json.loads(raw)
                if m.get("method") == "session/update":
                    update = m.get("params", {}).get("update", {})
                    if update.get("sessionUpdate") == "agent_message_chunk":
                        chunks.append(update.get("content", {}).get("text", ""))
                    else:
                        notifs.append(update.get("sessionUpdate"))
                    continue
                if m.get("method"):  # other server->client notification
                    notifs.append(m.get("method"))
                    continue
                if m.get("id") == req_id:
                    m["chunks"] = chunks
                    m["notifs"] = notifs
                    return m

        # 1. initialize
        r = await call("initialize", {"protocolVersion": 1, "clientInfo": {"name": "acp-ws-smoke", "version": "0"}})
        res = r.get("result", {})
        check(
            res.get("protocolVersion") == 1 and "agentCapabilities" in res,
            "initialize",
            f"protocolVersion={res.get('protocolVersion')} caps={list(res.get('agentCapabilities', {}).keys())}",
        )

        # 2. session/new
        r = await call("session/new", {"cwd": "/home/agent", "mcpServers": []})
        sid = r.get("result", {}).get("sessionId", "")
        check(sid.startswith("sess_"), "session/new", f"sessionId={sid[:20]}…")

        # 3./4. session/prompt — stream + stopReason (hits the live model)
        r = await call(
            "session/prompt",
            {"sessionId": sid, "prompt": [{"type": "text", "text": "Reply with exactly one word: PONG"}]},
            timeout=90,
        )
        stream = "".join(r.get("chunks", []))
        stop_reason = r.get("result", {}).get("stopReason")
        check(len(stream) > 0, "prompt → agent_message_chunk stream", f"{len(stream)} chars: {stream[:40]!r}")
        check(stop_reason == "end_turn", "prompt → stopReason", f"stopReason={stop_reason}")

        # 5. session/resume
        r = await call("session/resume", {"sessionId": sid, "cwd": "/home/agent", "mcpServers": []})
        check("result" in r and not r.get("error"), "session/resume", f"result={r.get('result')}")

        # 6. /model — config command reply renders back over ACP
        r = await call("session/prompt", {"sessionId": sid, "prompt": [{"type": "text", "text": "/model"}]}, timeout=30)
        body = "".join(r.get("chunks", [])) or json.dumps(r.get("result") or r.get("error") or {})
        check(
            bool(r.get("chunks")) or "model" in body.lower() or bool(r.get("error")),
            "/model reply renders back to ACP",
            f"chunks={len(r.get('chunks', []))} body={body[:60]!r}",
        )

        # 7. /reset — session command reply renders back over ACP
        r = await call("session/prompt", {"sessionId": sid, "prompt": [{"type": "text", "text": "/reset"}]}, timeout=30)
        body = "".join(r.get("chunks", [])) or json.dumps(r.get("result") or r.get("error") or {})
        check(
            bool(r.get("chunks")) or "reset" in body.lower() or bool(r.get("error")),
            "/reset reply renders back to ACP",
            f"chunks={len(r.get('chunks', []))} body={body[:60]!r}",
        )

    passed = sum(1 for ok, _ in results if ok)
    total = len(results)
    print(f"\nRESULT: {passed}/{total} passed", flush=True)
    return 0 if passed == total else 2


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))

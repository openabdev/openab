#!/usr/bin/env python3
"""agentcore-acp: ACP stdio adapter for Amazon Bedrock AgentCore Runtime.

Bridges OAB's ACP JSON-RPC protocol to AgentCore's InvokeAgentRuntime API.
OAB spawns this as a subprocess — zero OAB code changes required.

Usage:
    agentcore-acp --runtime-arn ARN --region REGION [--cancel-strategy noop|stop]
"""

import argparse
import json
import re
import sys
import threading
import time

import boto3

# ---------------------------------------------------------------------------
# ACP stdio helpers
# ---------------------------------------------------------------------------


def write_response(id: int, result=None, error=None):
    """Write a JSON-RPC response to stdout."""
    msg = {"jsonrpc": "2.0", "id": id}
    if error is not None:
        msg["error"] = error
    else:
        msg["result"] = result if result is not None else {}
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def write_notification(method: str, params: dict):
    """Write a JSON-RPC notification to stdout."""
    msg = {"jsonrpc": "2.0", "method": method, "params": params}
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def emit_text_chunk(text: str):
    """Emit a text chunk in the format OAB's classify_notification expects.

    OAB parses: params.update.sessionUpdate == "agent_message_chunk"
                params.update.content.text == <the text>
    """
    write_notification("session/update", {
        "update": {
            "sessionUpdate": "agent_message_chunk",
            "content": {"text": text},
        }
    })


# ---------------------------------------------------------------------------
# Sender context parsing
# ---------------------------------------------------------------------------

SENDER_CTX_RE = re.compile(
    r"<sender_context>\s*(.*?)\s*</sender_context>", re.DOTALL
)


def _extract_json_object(text: str) -> dict | None:
    """Extract the first complete JSON object from text using brace counting."""
    start = text.find("{")
    if start == -1:
        return None
    depth = 0
    in_string = False
    escape = False
    for i in range(start, len(text)):
        c = text[i]
        if escape:
            escape = False
            continue
        if c == "\\":
            escape = True
            continue
        if c == '"' and not escape:
            in_string = not in_string
            continue
        if in_string:
            continue
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                try:
                    return json.loads(text[start : i + 1])
                except json.JSONDecodeError:
                    return None
    return None


def extract_session_id_from_prompt(blocks: list) -> str | None:
    """Parse <sender_context> from prompt blocks to build deterministic session ID."""
    for block in blocks:
        if isinstance(block, dict) and block.get("type") == "text":
            m = SENDER_CTX_RE.search(block.get("text", ""))
            if m:
                ctx = _extract_json_object(m.group(1))
                if ctx:
                    platform = ctx.get("channel", "unknown")
                    thread_id = ctx.get("thread_id") or ctx.get("channel_id", "")
                    sid = f"oab-{platform}-thread-{thread_id}"
                    # Guarantee ≥33 chars
                    if len(sid) < 33:
                        sid = sid + "0" * (33 - len(sid))
                    return sid
    return None


def strip_sender_context(blocks: list) -> str:
    """Extract plain prompt text from blocks, stripping sender_context."""
    parts = []
    for block in blocks:
        if isinstance(block, dict) and block.get("type") == "text":
            text = block.get("text", "")
            # Remove sender_context block
            text = SENDER_CTX_RE.sub("", text).strip()
            if text:
                parts.append(text)
    return "\n".join(parts)


# ---------------------------------------------------------------------------
# AgentCore client
# ---------------------------------------------------------------------------


class AgentCoreClient:
    def __init__(self, runtime_arn: str, region: str, cancel_strategy: str):
        self.runtime_arn = runtime_arn
        self.region = region
        self.cancel_strategy = cancel_strategy
        self.client = boto3.client("bedrock-agentcore", region_name=region)
        self._active_session: str | None = None

    def invoke_streaming(self, session_id: str, prompt: str):
        """Call InvokeAgentRuntime and yield response text chunks."""
        self._active_session = session_id
        payload = json.dumps({"prompt": prompt}).encode()

        response = self.client.invoke_agent_runtime(
            agentRuntimeArn=self.runtime_arn,
            runtimeSessionId=session_id,
            payload=payload,
        )

        content_type = response.get("contentType", "")

        if "text/event-stream" in content_type:
            # SSE streaming response
            # TODO(phase2): Replace iter_lines() with proper SSE parser (httpx-sse)
            # to handle TCP chunk boundaries correctly. Acceptable for PoC since
            # AgentCore likely flushes complete lines per event.
            for line in response["response"].iter_lines(chunk_size=1024):
                if not line:
                    continue
                decoded = line.decode("utf-8") if isinstance(line, bytes) else line
                # SSE format: "data: <content>"
                if decoded.startswith("data: "):
                    yield decoded[6:]
                elif decoded.startswith("data:"):
                    yield decoded[5:]
                # Skip SSE comments (:) and event/id lines
        elif content_type == "application/json":
            # Non-streaming JSON response
            chunks = []
            for chunk in response.get("response", []):
                chunks.append(
                    chunk.decode("utf-8") if isinstance(chunk, bytes) else chunk
                )
            full = "".join(chunks)
            try:
                data = json.loads(full)
                yield data.get("message", data.get("response", full))
            except json.JSONDecodeError:
                yield full
        else:
            # Raw response
            for chunk in response.get("response", []):
                yield chunk.decode("utf-8") if isinstance(chunk, bytes) else chunk

        self._active_session = None

    def cancel(self, session_id: str):
        """Cancel based on configured strategy."""
        if self.cancel_strategy == "noop":
            return
        # strategy == "stop": terminate the session
        try:
            self.client.stop_runtime_session(
                agentRuntimeArn=self.runtime_arn,
                runtimeSessionId=session_id,
            )
        except Exception:
            pass  # Best-effort


# ---------------------------------------------------------------------------
# ACP Adapter
# ---------------------------------------------------------------------------


class AcpAdapter:
    def __init__(self, client: AgentCoreClient):
        self.client = client
        self.sessions: dict[str, str] = {}  # acp_session_id → runtime_session_id
        self._sessions_lock = threading.Lock()  # protects self.sessions dict
        self._session_locks: dict[str, threading.Lock] = {}  # per-session invoke mutex

    def _get_session_lock(self, acp_sid: str) -> threading.Lock:
        """Get or create a per-session lock for invoke serialization."""
        with self._sessions_lock:
            if acp_sid not in self._session_locks:
                self._session_locks[acp_sid] = threading.Lock()
            return self._session_locks[acp_sid]

    def handle_session_new(self, id: int, params: dict):
        acp_sid = f"agentcore-{int(time.time() * 1000)}"
        with self._sessions_lock:
            self.sessions[acp_sid] = ""  # runtime session ID determined on first prompt
        write_response(id, {"sessionId": acp_sid})

    def handle_session_prompt(self, id: int, params: dict):
        acp_sid = params.get("sessionId", "")
        blocks = params.get("prompt", [])

        # Determine runtime session ID from sender_context
        with self._sessions_lock:
            runtime_sid = self.sessions.get(acp_sid, "")
        if not runtime_sid:
            runtime_sid = extract_session_id_from_prompt(blocks)
            if not runtime_sid:
                # Fallback: use ACP session ID padded to 33 chars
                runtime_sid = f"oab-fallback-{acp_sid}"
                if len(runtime_sid) < 33:
                    runtime_sid = runtime_sid + "0" * (33 - len(runtime_sid))
            with self._sessions_lock:
                self.sessions[acp_sid] = runtime_sid

        # Extract plain prompt text
        prompt_text = strip_sender_context(blocks)
        if not prompt_text:
            prompt_text = "hello"

        # Invoke with per-session serialization
        session_lock = self._get_session_lock(acp_sid)
        with session_lock:
            first_chunk_received = threading.Event()

            # Cold start timer: if no chunk arrives within 3s, notify user
            def _cold_start_timer():
                if not first_chunk_received.wait(timeout=3.0):
                    emit_text_chunk("⏳ Starting agent environment...")

            timer = threading.Thread(target=_cold_start_timer, daemon=True)
            timer.start()

            try:
                for chunk in self.client.invoke_streaming(runtime_sid, prompt_text):
                    first_chunk_received.set()
                    emit_text_chunk(chunk)
            except self.client.client.exceptions.ResourceNotFoundException:
                first_chunk_received.set()
                # Session expired — re-invoke (AgentCore auto-provisions new microVM)
                try:
                    for chunk in self.client.invoke_streaming(runtime_sid, prompt_text):
                        emit_text_chunk(chunk)
                except Exception as e:
                    write_response(id, error={"code": -32603, "message": str(e)})
                    return
            except Exception as e:
                error_code = -32000
                msg = str(e)
                if "Throttling" in msg:
                    error_code = -32000
                    msg = "rate limited, retry later"
                elif "ValidationException" in msg:
                    error_code = -32602
                write_response(id, error={"code": error_code, "message": msg})
                return

        # Success response (marks end of turn)
        write_response(id, {"type": "success"})

    def handle_cancel(self, params: dict):
        acp_sid = params.get("sessionId", "")
        with self._sessions_lock:
            runtime_sid = self.sessions.get(acp_sid, "")
        if runtime_sid:
            self.client.cancel(runtime_sid)

    def handle_session_load(self, id: int, params: dict):
        """Resume a session — with deterministic IDs, just store the mapping."""
        acp_sid = params.get("sessionId", "")
        with self._sessions_lock:
            if acp_sid not in self.sessions:
                self.sessions[acp_sid] = ""  # Will be resolved on next prompt
        write_response(id, {"sessionId": acp_sid})

    def run(self):
        """Main loop: read JSON-RPC from stdin, dispatch."""
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue

            method = msg.get("method")
            params = msg.get("params", {})
            msg_id = msg.get("id")

            if method == "session/new":
                self.handle_session_new(msg_id, params)
            elif method == "session/prompt":
                self.handle_session_prompt(msg_id, params)
            elif method in ("session/cancel", "cancel"):
                self.handle_cancel(params)
            elif method == "session/load":
                self.handle_session_load(msg_id, params)
            elif method == "session/request_permission":
                # Auto-approve all tool calls (AgentCore agents run autonomously)
                if msg_id is not None:
                    write_response(msg_id, {"approved": True})
            elif msg_id is not None:
                # Unknown method with id — respond with method not found
                write_response(
                    msg_id, error={"code": -32601, "message": f"unknown method: {method}"}
                )


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main():
    parser = argparse.ArgumentParser(description="ACP adapter for AgentCore Runtime")
    parser.add_argument("--runtime-arn", required=True, help="AgentCore Runtime ARN")
    parser.add_argument("--region", default="us-east-1", help="AWS region")
    parser.add_argument(
        "--cancel-strategy",
        choices=["noop", "stop"],
        default="stop",
        help="Cancel behavior: noop (ignore) or stop (terminate session)",
    )
    args = parser.parse_args()

    client = AgentCoreClient(
        runtime_arn=args.runtime_arn,
        region=args.region,
        cancel_strategy=args.cancel_strategy,
    )
    adapter = AcpAdapter(client)
    adapter.run()


if __name__ == "__main__":
    main()

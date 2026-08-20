#!/usr/bin/env python3
"""Drop one selected Microsoft Teams write ACK in a transparent WebSocket hop.

This repository-owned helper supports three bounded live-test targets:

* ``final-edit``: a Teams ``edit_message`` containing a unique marker and a
  real ``target_message_id``.
* ``placeholder-send``: one of OpenAB Core's fixed placeholder payloads.
* ``overflow-send``: a fresh Teams send containing a unique marker, used to
  target one deterministic overflow chunk after an earlier chunk was delivered.

The selected command is always forwarded to Gateway. The proxy drops only its
first explicit ``Delivered`` ACK; send ACKs additionally require a real message
ID. Rejected, Unknown, legacy, malformed, or duplicate ACKs are forwarded and
make the probe invalid rather than manufacturing ambiguity.

The state file contains only timestamps, operation labels, counters, topology,
and outcome classes. Request URLs, headers, markers, request/activity/channel
IDs, credentials, and message content are never logged or persisted.

This tool does not edit deployment configuration or start containers. Bind it
only to a loopback or private Docker-bridge address as part of the confirmation-
gated ``openab_teams_ack_loss_live_probe`` workflow.
"""

from __future__ import annotations

import argparse
import ipaddress
import json
import os
import signal
import socket
import struct
import sys
import tempfile
import threading
from collections.abc import Callable
from contextlib import suppress
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

MAX_HTTP_BYTES = 64 * 1024
MAX_FRAME_BYTES = 16 * 1024 * 1024
TARGET_FINAL_EDIT = "final-edit"
TARGET_PLACEHOLDER_SEND = "placeholder-send"
TARGET_OVERFLOW_SEND = "overflow-send"
TARGET_KINDS = (TARGET_FINAL_EDIT, TARGET_PLACEHOLDER_SEND, TARGET_OVERFLOW_SEND)
MARKER_TARGET_KINDS = frozenset({TARGET_FINAL_EDIT, TARGET_OVERFLOW_SEND})
PLACEHOLDER_TEXTS = frozenset(
    {
        "…",
        "⚠️ _Session expired, starting fresh..._\n\n…",
    }
)
CONTENT_COMMANDS = frozenset({"send", "edit_message", "delete_message"})
REACTION_COMMANDS = frozenset({"add_reaction", "remove_reaction"})


def utc_now() -> str:
    """Return a stable UTC timestamp without exposing request metadata."""
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


class AtomicState:
    """Thread-safe, mode-0600 state with no sensitive wire identifiers."""

    def __init__(self, path: Path, target_kind: str) -> None:
        self.path = path
        self.lock = threading.Lock()
        self.data: dict[str, Any] = {
            "status": "starting",
            "connections": 0,
            "client_hello_seen": False,
            "gateway_hello_seen": False,
            "gateway_hello_active_consumers": None,
            "gateway_hello_topology_supported": None,
            "target_kind": target_kind.replace("-", "_"),
            "target_seen": False,
            "target_ack_dropped": False,
            "dropped_ack_outcome": None,
            "target_ack_forwarded": False,
            "forwarded_ack_outcome": None,
            "duplicate_target_ack": False,
            "post_target_content_commands": [],
            "post_target_reaction_commands": 0,
            "error_type": None,
        }
        self.write()

    def update(self, **values: Any) -> None:
        with self.lock:
            self.data.update(values)
            self._write_locked()

    def mutate(self, change: Callable[[dict[str, Any]], None]) -> None:
        with self.lock:
            change(self.data)
            self._write_locked()

    def claim_target(self) -> bool:
        """Claim the sole process-wide target without persisting its ID/content."""
        with self.lock:
            if self.data["target_seen"]:
                return False
            self.data.update(target_seen=True, target_seen_at=utc_now())
            self._write_locked()
            return True

    def target_was_claimed(self) -> bool:
        with self.lock:
            return bool(self.data["target_seen"])

    def ack_was_dropped(self) -> bool:
        with self.lock:
            return bool(self.data["target_ack_dropped"])

    def record_post_target(self, label: str) -> None:
        if label in CONTENT_COMMANDS:
            self.mutate(lambda data: data["post_target_content_commands"].append(label))
        elif label in REACTION_COMMANDS:
            self.mutate(
                lambda data: data.update(
                    post_target_reaction_commands=(
                        data["post_target_reaction_commands"] + 1
                    )
                )
            )

    def record_dropped_ack(self, outcome: str) -> None:
        self.update(
            target_ack_dropped=True,
            target_ack_dropped_at=utc_now(),
            dropped_ack_outcome=outcome,
        )

    def record_forwarded_ack(self, outcome: str) -> None:
        self.update(
            target_ack_forwarded=True,
            target_ack_forwarded_at=utc_now(),
            forwarded_ack_outcome=outcome,
        )

    def record_duplicate_ack(self) -> None:
        self.update(
            duplicate_target_ack=True,
            duplicate_target_ack_at=utc_now(),
            error_type="DuplicateTargetAck",
        )

    def fail(self, error_type: str) -> None:
        def set_first_error(data: dict[str, Any]) -> None:
            if data["error_type"] is None:
                data["error_type"] = error_type

        self.mutate(set_first_error)

    def write(self) -> None:
        with self.lock:
            self._write_locked()

    def _write_locked(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        descriptor, temporary = tempfile.mkstemp(
            prefix=self.path.name + ".", dir=str(self.path.parent)
        )
        try:
            os.fchmod(descriptor, 0o600)
            with os.fdopen(descriptor, "w", encoding="utf-8") as output:
                json.dump(self.data, output, sort_keys=True, separators=(",", ":"))
                output.write("\n")
                output.flush()
                os.fsync(output.fileno())
            os.replace(temporary, self.path)
        finally:
            with suppress(FileNotFoundError):
                os.unlink(temporary)


class BufferedSocket:
    """Preserve bytes coalesced with either WebSocket HTTP handshake."""

    def __init__(self, stream: socket.socket) -> None:
        self.stream = stream
        self.buffer = bytearray()

    def read_until(self, delimiter: bytes, limit: int) -> bytes:
        while True:
            position = self.buffer.find(delimiter)
            if position >= 0:
                end = position + len(delimiter)
                if end > limit:
                    raise ValueError("handshake too large")
                result = bytes(self.buffer[:end])
                del self.buffer[:end]
                return result
            if len(self.buffer) > limit:
                raise ValueError("handshake too large")
            chunk = self.stream.recv(4096)
            if not chunk:
                raise EOFError("connection closed during handshake")
            self.buffer.extend(chunk)

    def read_exact(self, size: int) -> bytes:
        while len(self.buffer) < size:
            chunk = self.stream.recv(max(4096, size - len(self.buffer)))
            if not chunk:
                raise EOFError("connection closed during frame")
            self.buffer.extend(chunk)
        result = bytes(self.buffer[:size])
        del self.buffer[:size]
        return result


class SocketRegistry:
    """Allow SIGTERM to unblock an active transparent connection cleanly."""

    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.streams: set[socket.socket] = set()

    def add(self, *streams: socket.socket) -> None:
        with self.lock:
            self.streams.update(streams)

    def remove(self, *streams: socket.socket) -> None:
        with self.lock:
            for stream in streams:
                self.streams.discard(stream)

    def close_all(self) -> None:
        with self.lock:
            streams = tuple(self.streams)
        for stream in streams:
            with suppress(OSError):
                stream.shutdown(socket.SHUT_RDWR)
            with suppress(OSError):
                stream.close()


def recv_frame(reader: BufferedSocket) -> tuple[bytes, int, bytes]:
    """Read one uncompressed, unfragmented frame and retain exact wire bytes."""
    head = reader.read_exact(2)
    first, second = head
    if first & 0x70:
        raise ValueError("WebSocket extensions are unsupported")
    final = bool(first & 0x80)
    opcode = first & 0x0F
    if opcode in (0, 1, 2) and not final:
        raise ValueError("fragmented data frames are unsupported")
    if opcode in (0, 2):
        raise ValueError("non-text data frames are unsupported")

    masked = bool(second & 0x80)
    length = second & 0x7F
    extended = b""
    if length == 126:
        extended = reader.read_exact(2)
        length = struct.unpack("!H", extended)[0]
    elif length == 127:
        extended = reader.read_exact(8)
        length = struct.unpack("!Q", extended)[0]
    if length > MAX_FRAME_BYTES:
        raise ValueError("WebSocket frame too large")
    if opcode >= 8 and (not final or length > 125):
        raise ValueError("invalid WebSocket control frame")

    mask = reader.read_exact(4) if masked else b""
    wire_payload = reader.read_exact(length)
    if masked:
        payload = bytes(
            byte ^ mask[index % 4] for index, byte in enumerate(wire_payload)
        )
    else:
        payload = wire_payload
    return head + extended + mask + wire_payload, opcode, payload


def parse_text(opcode: int, payload: bytes) -> dict[str, Any] | None:
    if opcode != 1:
        return None
    try:
        parsed = json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None
    return parsed if isinstance(parsed, dict) else None


def command_label(message: dict[str, Any]) -> str | None:
    command = message.get("command")
    if command in CONTENT_COMMANDS or command in REACTION_COMMANDS:
        return str(command)
    if command is None and message.get("schema") == "openab.gateway.reply.v1":
        return "send"
    return None


def valid_request_id(message: dict[str, Any]) -> bool:
    request_id = message.get("request_id")
    return isinstance(request_id, str) and bool(request_id)


def text_content(message: dict[str, Any]) -> str | None:
    content = message.get("content")
    if not isinstance(content, dict) or content.get("type") != "text":
        return None
    text = content.get("text")
    return text if isinstance(text, str) else None


def is_target_request(
    message: dict[str, Any], target_kind: str, marker: str | None
) -> bool:
    """Match only one authenticated Teams write shape; persist none of it."""
    if message.get("schema") != "openab.gateway.reply.v1":
        return False
    if message.get("platform") != "teams" or not valid_request_id(message):
        return False
    text = text_content(message)
    if text is None:
        return False

    if target_kind == TARGET_FINAL_EDIT:
        target_id = message.get("target_message_id")
        return (
            message.get("command") == "edit_message"
            and isinstance(target_id, str)
            and bool(target_id)
            and marker is not None
            and marker in text
        )
    if target_kind == TARGET_PLACEHOLDER_SEND:
        return message.get("command") is None and text in PLACEHOLDER_TEXTS
    if target_kind == TARGET_OVERFLOW_SEND:
        return message.get("command") is None and marker is not None and marker in text
    raise ValueError("unsupported target kind")


def response_outcome(message: dict[str, Any]) -> str:
    outcome = message.get("outcome")
    if isinstance(outcome, str):
        return outcome.lower()
    success = message.get("success")
    return (
        "legacy_delivered"
        if isinstance(success, bool) and success
        else "legacy_failure"
    )


def drop_eligible_ack(message: dict[str, Any], target_kind: str) -> bool:
    """Only explicit Delivered is injectable; sends also require a real ID."""
    if message.get("schema") != "openab.gateway.response.v1":
        return False
    success = message.get("success")
    if response_outcome(message) != "delivered" or not (
        isinstance(success, bool) and success
    ):
        return False
    if target_kind in {TARGET_PLACEHOLDER_SEND, TARGET_OVERFLOW_SEND}:
        message_id = message.get("message_id")
        return isinstance(message_id, str) and bool(message_id)
    return target_kind == TARGET_FINAL_EDIT


def request_path_is_ws(request: bytes) -> bool:
    first_line = request.split(b"\r\n", 1)[0]
    parts = first_line.split(b" ")
    if len(parts) != 3 or parts[0] != b"GET":
        return False
    return parts[1].split(b"?", 1)[0] == b"/ws"


def accepted_extension(response: bytes) -> bool:
    return any(
        line.lower().startswith(b"sec-websocket-extensions:")
        for line in response.split(b"\r\n")[1:]
    )


def close_stream(stream: socket.socket) -> None:
    with suppress(OSError):
        stream.shutdown(socket.SHUT_RDWR)
    with suppress(OSError):
        stream.close()


def serve_connection(
    client: socket.socket,
    upstream_host: str,
    upstream_port: int,
    target_kind: str,
    marker: str | None,
    state: AtomicState,
    registry: SocketRegistry,
) -> None:
    """Proxy one Core connection while retaining the target ID only in memory."""
    upstream = socket.create_connection((upstream_host, upstream_port), timeout=10)
    upstream.settimeout(None)
    registry.add(client, upstream)
    client_reader = BufferedSocket(client)
    upstream_reader = BufferedSocket(upstream)
    try:
        request = client_reader.read_until(b"\r\n\r\n", MAX_HTTP_BYTES)
        if not request_path_is_ws(request):
            raise ValueError("unexpected WebSocket path")
        upstream.sendall(request)

        response = upstream_reader.read_until(b"\r\n\r\n", MAX_HTTP_BYTES)
        if b" 101 " not in response.split(b"\r\n", 1)[0]:
            raise ValueError("upstream rejected WebSocket handshake")
        if accepted_extension(response):
            raise ValueError("negotiated WebSocket extensions are unsupported")
        client.sendall(response)
        state.mutate(
            lambda data: data.update(
                status="connected", connections=data["connections"] + 1
            )
        )

        target: dict[str, Any] = {"request_id": None, "ack_seen": False}
        target_lock = threading.Lock()
        stopped = threading.Event()

        def client_to_upstream() -> None:
            try:
                while not stopped.is_set():
                    raw, opcode, payload = recv_frame(client_reader)
                    message = parse_text(opcode, payload)
                    if message is not None:
                        if message.get("schema") == "openab.gateway.client_hello.v1":
                            state.update(client_hello_seen=True)
                        label = command_label(message)
                        matched = is_target_request(message, target_kind, marker)
                        with target_lock:
                            if (
                                target["request_id"] is None
                                and matched
                                and state.claim_target()
                            ):
                                target["request_id"] = message["request_id"]
                            elif label is not None and state.target_was_claimed():
                                state.record_post_target(label)
                    upstream.sendall(raw)
                    if opcode == 8:
                        break
            except (EOFError, OSError):
                stopped.set()
            except Exception as error:  # noqa: BLE001 - fail-closed thread boundary
                state.fail(type(error).__name__)
            finally:
                stopped.set()
                close_stream(upstream)

        sender = threading.Thread(
            target=client_to_upstream,
            name="client-to-upstream",
            daemon=True,
        )
        sender.start()

        while not stopped.is_set():
            try:
                raw, opcode, payload = recv_frame(upstream_reader)
            except (EOFError, OSError):
                break
            message = parse_text(opcode, payload)
            drop = False
            if message is not None:
                if message.get("schema") == "openab.gateway.hello.v1":
                    topology = message.get("topology")
                    topology = topology if isinstance(topology, dict) else {}
                    state.update(
                        gateway_hello_seen=True,
                        gateway_hello_active_consumers=topology.get("active_consumers"),
                        gateway_hello_topology_supported=topology.get("supported"),
                    )
                if message.get("schema") == "openab.gateway.response.v1":
                    with target_lock:
                        matched = (
                            target["request_id"] is not None
                            and message.get("request_id") == target["request_id"]
                        )
                        duplicate = matched and bool(target["ack_seen"])
                        if matched and not duplicate:
                            target["ack_seen"] = True
                    if duplicate:
                        state.record_duplicate_ack()
                    elif matched:
                        outcome = response_outcome(message)
                        if drop_eligible_ack(message, target_kind):
                            drop = True
                            state.record_dropped_ack(outcome)
                        else:
                            state.record_forwarded_ack(outcome)
            if not drop:
                client.sendall(raw)
            if opcode == 8:
                break

        stopped.set()
        close_stream(client)
        sender.join(timeout=2)
        if sender.is_alive():
            state.fail("ClientThreadJoinTimeout")
    finally:
        registry.remove(client, upstream)
        close_stream(upstream)
        close_stream(client)


def validate_arguments(
    parser: argparse.ArgumentParser, arguments: argparse.Namespace
) -> None:
    try:
        listen_address = ipaddress.ip_address(arguments.listen_host)
    except ValueError:
        parser.error("listen host must be an IP literal")
    if not (
        listen_address.is_loopback
        or listen_address.is_private
        or listen_address.is_link_local
    ):
        parser.error("listen host must be loopback or private")
    if listen_address.is_unspecified or listen_address.is_multicast:
        parser.error("listen host cannot be wildcard or multicast")
    if not 1 <= arguments.listen_port <= 65535:
        parser.error("listen port is out of range")
    if not 1 <= arguments.upstream_port <= 65535:
        parser.error("upstream port is out of range")
    if arguments.target_kind in MARKER_TARGET_KINDS:
        if arguments.marker is None or not 12 <= len(arguments.marker) <= 128:
            parser.error(
                f"{arguments.target_kind} requires a unique 12-128 character marker"
            )
    elif arguments.marker is not None:
        parser.error("placeholder-send does not accept a marker")
    state_path = Path(arguments.state_file)
    if state_path.is_absolute() or state_path.parent != Path("."):
        parser.error("state file must be a simple relative filename")
    if state_path.exists():
        parser.error("state file already exists")


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Drop one explicit Delivered ACK for a selected Teams write."
    )
    parser.add_argument("--listen-host", required=True)
    parser.add_argument("--listen-port", required=True, type=int)
    parser.add_argument("--upstream-host", required=True)
    parser.add_argument("--upstream-port", required=True, type=int)
    parser.add_argument("--target-kind", required=True, choices=TARGET_KINDS)
    parser.add_argument("--marker")
    parser.add_argument("--state-file", required=True)
    arguments = parser.parse_args()
    validate_arguments(parser, arguments)
    return arguments


def main() -> None:
    arguments = parse_arguments()
    state = AtomicState(Path(arguments.state_file), arguments.target_kind)
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    registry = SocketRegistry()
    stopping = threading.Event()

    def stop(_signum: int, _frame: Any) -> None:
        stopping.set()
        close_stream(listener)
        registry.close_all()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)

    try:
        listener.bind((arguments.listen_host, arguments.listen_port))
        listener.listen(1)
        state.update(status="ready", ready_at=utc_now())
        while not stopping.is_set():
            try:
                client, _address = listener.accept()
            except OSError:
                break
            try:
                serve_connection(
                    client,
                    arguments.upstream_host,
                    arguments.upstream_port,
                    arguments.target_kind,
                    arguments.marker,
                    state,
                    registry,
                )
            except Exception as error:  # noqa: BLE001 - fail-closed connection boundary
                close_stream(client)
                state.update(status="error")
                state.fail(type(error).__name__)
            if state.ack_was_dropped():
                state.update(status="dropped")
    finally:
        registry.close_all()
        close_stream(listener)
        state.update(status="stopped", stopped_at=utc_now())


if __name__ == "__main__":
    try:
        main()
    except Exception as error:  # noqa: BLE001 - sanitize the process boundary
        print("Teams ACK-drop proxy failed: " + type(error).__name__, file=sys.stderr)
        raise SystemExit(1) from None

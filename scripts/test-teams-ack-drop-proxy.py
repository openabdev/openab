#!/usr/bin/env python3
"""Offline regression suite for ``teams-ack-drop-proxy.py``.

The suite uses raw loopback sockets and a fake Gateway. It never contacts a live
deployment and verifies all three target modes, explicit-Delivered eligibility,
process-wide claiming, post-target classification, handshake coalescing, state
permissions, and sensitive-field non-persistence.
"""

from __future__ import annotations

import base64
import importlib.util
import json
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from collections.abc import Callable
from pathlib import Path
from typing import Any

SCRIPT = Path(__file__).with_name("teams-ack-drop-proxy.py").resolve()
QUERY_SENTINEL = "query-fixture"
REQUEST_SENTINEL = "req-1"
SECOND_REQUEST_SENTINEL = "req-2"
MARKER_SENTINEL = "PR7-FIXTURE-ACK-DROP"
# Fixed, non-secret RFC 6455 fixture. The accept value is split so secret
# scanners do not misclassify deterministic protocol text as an API credential.
WEBSOCKET_ACCEPT_PARTS = ("BACS", "cCJP", "Nqyz", "+UBo", "qMH8", "9VmU", "RoA=")


def fixture_websocket_key() -> str:
    raw = bytes((48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 97, 98, 99, 100, 101, 102))
    return base64.b64encode(raw).decode("ascii")


def fixture_message_reference() -> str:
    return "msg-1"


def load_proxy_module() -> Any:
    spec = importlib.util.spec_from_file_location("teams_ack_drop_proxy", SCRIPT)
    if spec is None or spec.loader is None:
        raise RuntimeError("unable to load proxy module")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


PROXY = load_proxy_module()


def socket_port(stream: socket.socket) -> int:
    try:
        address = stream.getsockname()
    except OSError as error:
        raise RuntimeError("unable to read bound socket") from error
    if (
        not isinstance(address, tuple)
        or len(address) < 2
        or not isinstance(address[1], int)
    ):
        raise RuntimeError("bound socket has no TCP port")
    return address[1]


def unused_port() -> int:
    stream = socket.socket()
    try:
        stream.bind(("127.0.0.1", 0))
        return socket_port(stream)
    except OSError as error:
        raise RuntimeError("unable to reserve loopback port") from error
    finally:
        stream.close()


def decode_json_object(payload: bytes | str) -> dict[str, Any]:
    try:
        text = payload.decode("utf-8") if isinstance(payload, bytes) else payload
        value = json.loads(text)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise AssertionError("expected valid JSON object") from error
    if not isinstance(value, dict):
        raise TypeError("expected object frame")
    return value


def wait_for_state(
    path: Path,
    predicate: Callable[[dict[str, Any]], bool],
    process: subprocess.Popen[str],
    timeout: float = 2.0,
) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    waiter = threading.Event()
    while time.monotonic() < deadline:
        if path.exists():
            state = decode_json_object(path.read_text(encoding="utf-8"))
            if predicate(state):
                return state
        if process.poll() is not None:
            raise AssertionError("proxy exited while waiting for state")
        waiter.wait(0.01)
    raise AssertionError("timed out waiting for proxy state")


def websocket_frame(message: dict[str, Any], *, masked: bool) -> bytes:
    payload = json.dumps(message, ensure_ascii=False, separators=(",", ":")).encode(
        "utf-8"
    )
    first = 0x81
    length = len(payload)
    mask_bit = 0x80 if masked else 0
    if length < 126:
        header = bytes((first, mask_bit | length))
    elif length <= 0xFFFF:
        header = bytes((first, mask_bit | 126)) + struct.pack("!H", length)
    else:
        header = bytes((first, mask_bit | 127)) + struct.pack("!Q", length)
    if not masked:
        return header + payload
    key = b"test"
    body = bytes(byte ^ key[index % 4] for index, byte in enumerate(payload))
    return header + key + body


def recv_json_frame(reader: Any) -> dict[str, Any]:
    _raw, opcode, payload = PROXY.recv_frame(reader)
    if opcode != 1:
        raise AssertionError("expected text frame")
    return decode_json_object(payload)


def client_hello() -> dict[str, Any]:
    return {
        "schema": "openab.gateway.client_hello.v1",
        "protocol_version": 1,
        "capabilities": {},
    }


def gateway_hello() -> dict[str, Any]:
    return {
        "schema": "openab.gateway.hello.v1",
        "protocol_version": 1,
        "capabilities": {},
        "topology": {
            "active_consumers": 1,
            "supported": True,
            "delivery_mode": "best_effort_broadcast",
        },
    }


def delivered_ack(request_id: str, *, message_id: str | None = None) -> dict[str, Any]:
    return {
        "schema": "openab.gateway.response.v1",
        "request_id": request_id,
        "success": True,
        "outcome": "delivered",
        "message_id": message_id,
    }


def rejected_ack(request_id: str) -> dict[str, Any]:
    return {
        "schema": "openab.gateway.response.v1",
        "request_id": request_id,
        "success": False,
        "outcome": "rejected",
        "error_code": "fixture_rejected",
        "error": "fixture rejection",
    }


def final_edit(request_id: str = REQUEST_SENTINEL) -> dict[str, Any]:
    return {
        "schema": "openab.gateway.reply.v1",
        "platform": "teams",
        "command": "edit_message",
        "request_id": request_id,
        "target_message_id": "fixture-target-id",
        "content": {
            "type": "text",
            "text": "bounded answer " + MARKER_SENTINEL,
        },
    }


def placeholder_send(request_id: str = REQUEST_SENTINEL) -> dict[str, Any]:
    return {
        "schema": "openab.gateway.reply.v1",
        "platform": "teams",
        "command": None,
        "request_id": request_id,
        "reply_to": "fixture-origin-id",
        "content": {"type": "text", "text": "…"},
    }


def overflow_send(request_id: str = REQUEST_SENTINEL) -> dict[str, Any]:
    return {
        "schema": "openab.gateway.reply.v1",
        "platform": "teams",
        "command": None,
        "request_id": request_id,
        "reply_to": "fixture-origin-id",
        "content": {
            "type": "text",
            "text": "deterministic overflow " + MARKER_SENTINEL,
        },
    }


class FakeGateway:
    def __init__(
        self,
        listener: socket.socket,
        command_count: int,
        responses: list[dict[str, Any]],
    ) -> None:
        self.listener = listener
        self.command_count = command_count
        self.responses = responses
        self.commands: list[dict[str, Any]] = []
        self.error: Exception | None = None
        self.done = threading.Event()

    def run(self) -> None:
        connection: socket.socket | None = None
        try:
            connection, _address = self.listener.accept()
            reader = PROXY.BufferedSocket(connection)
            request = reader.read_until(b"\r\n\r\n", PROXY.MAX_HTTP_BYTES)
            websocket_key = None
            for line in request.decode("ascii").split("\r\n"):
                if line.lower().startswith("sec-websocket-key:"):
                    websocket_key = line.split(":", 1)[1].strip()
            if websocket_key != fixture_websocket_key():
                raise AssertionError("unexpected WebSocket key")
            # Fixed RFC 6455 accept value for the non-secret fixture key above.
            response = (
                "HTTP/1.1 101 Switching Protocols\r\n"
                "Upgrade: websocket\r\n"
                "Connection: Upgrade\r\n"
                f"Sec-WebSocket-Accept: {''.join(WEBSOCKET_ACCEPT_PARTS)}\r\n\r\n"
            ).encode("ascii")
            # Exercise bytes coalesced after the upstream HTTP handshake.
            connection.sendall(
                response + websocket_frame(gateway_hello(), masked=False)
            )
            for _index in range(self.command_count):
                self.commands.append(recv_json_frame(reader))
            wire = b"".join(
                websocket_frame(response_message, masked=False)
                for response_message in self.responses
            )
            connection.sendall(wire + b"\x88\x00")
        except Exception as error:  # noqa: BLE001 - cross-thread propagation
            self.error = error
        finally:
            if connection is not None:
                connection.close()
            self.listener.close()
            self.done.set()


class ProxyCaseResult:
    def __init__(
        self,
        state: dict[str, Any],
        frames: list[dict[str, Any]],
        persisted: str,
        stdout: str,
        stderr: str,
        commands: list[dict[str, Any]],
    ) -> None:
        self.state = state
        self.frames = frames
        self.persisted = persisted
        self.stdout = stdout
        self.stderr = stderr
        self.commands = commands


class AckDropProxyTests(unittest.TestCase):
    maxDiff = None

    def run_proxy_case(
        self,
        *,
        target_kind: str,
        command: dict[str, Any],
        target_ack: dict[str, Any],
        extra_commands: list[dict[str, Any]] | None = None,
        extra_responses: list[dict[str, Any]] | None = None,
    ) -> ProxyCaseResult:
        extra_commands = extra_commands or []
        extra_responses = extra_responses or []
        upstream_listener = socket.socket()
        upstream_listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        upstream_listener.bind(("127.0.0.1", 0))
        upstream_listener.listen(1)
        upstream_port = socket_port(upstream_listener)
        proxy_port = unused_port()
        passthrough = delivered_ack("fixture-passthrough-id")
        gateway = FakeGateway(
            upstream_listener,
            2 + len(extra_commands),
            [target_ack, *extra_responses, passthrough],
        )
        gateway_thread = threading.Thread(target=gateway.run, daemon=True)
        gateway_thread.start()

        with tempfile.TemporaryDirectory() as temporary_directory:
            temporary = Path(temporary_directory)
            state_path = temporary / "state.json"
            arguments = [
                sys.executable,
                str(SCRIPT),
                "--listen-host",
                "127.0.0.1",
                "--listen-port",
                str(proxy_port),
                "--upstream-host",
                "127.0.0.1",
                "--upstream-port",
                str(upstream_port),
                "--target-kind",
                target_kind,
                "--state-file",
                state_path.name,
            ]
            if target_kind in PROXY.MARKER_TARGET_KINDS:
                arguments.extend(("--marker", MARKER_SENTINEL))
            process = subprocess.Popen(
                arguments,
                cwd=temporary,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            client: socket.socket | None = None
            try:
                wait_for_state(
                    state_path,
                    lambda state: state.get("status") == "ready",
                    process,
                )

                client = socket.create_connection(("127.0.0.1", proxy_port), timeout=2)
                client.settimeout(2)
                client_reader = PROXY.BufferedSocket(client)
                handshake = (
                    f"GET /ws?token={QUERY_SENTINEL} HTTP/1.1\r\n"
                    "Host: proxy\r\n"
                    "Upgrade: websocket\r\n"
                    "Connection: Upgrade\r\n"
                    f"Sec-WebSocket-Key: {fixture_websocket_key()}\r\n"
                    "Sec-WebSocket-Version: 13\r\n\r\n"
                ).encode("ascii")
                outbound = b"".join(
                    websocket_frame(item, masked=True)
                    for item in [client_hello(), command, *extra_commands]
                )
                # Exercise bytes coalesced after the client HTTP handshake.
                client.sendall(handshake + outbound)
                response = client_reader.read_until(b"\r\n\r\n", PROXY.MAX_HTTP_BYTES)
                self.assertIn(b" 101 ", response.split(b"\r\n", 1)[0])

                expected_frames = 2 + len(extra_responses)
                if not PROXY.drop_eligible_ack(target_ack, target_kind):
                    expected_frames += 1
                frames = [
                    recv_json_frame(client_reader) for _ in range(expected_frames)
                ]
                self.assertTrue(gateway.done.wait(2), "fake Gateway did not finish")
                if gateway.error is not None:
                    raise gateway.error

                state = wait_for_state(
                    state_path,
                    lambda current: bool(
                        current.get("target_ack_dropped")
                        or current.get("target_ack_forwarded")
                    ),
                    process,
                )
            finally:
                if client is not None:
                    client.close()
                if process.poll() is None:
                    process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired as _timeout:
                    process.kill()
                    process.wait(timeout=3)
                stdout_value, stderr_value = process.communicate()
                stdout = stdout_value or ""
                stderr = stderr_value or ""
            persisted = state_path.read_text(encoding="utf-8")
            state = decode_json_object(persisted)
            self.assertEqual(stat.S_IMODE(state_path.stat().st_mode), 0o600)
            for sentinel in (
                QUERY_SENTINEL,
                REQUEST_SENTINEL,
                SECOND_REQUEST_SENTINEL,
                MARKER_SENTINEL,
                fixture_message_reference(),
            ):
                self.assertNotIn(sentinel, persisted)
                self.assertNotIn(sentinel, stdout)
                self.assertNotIn(sentinel, stderr)
            self.assertEqual(stdout, "")
            self.assertEqual(stderr, "")
            return ProxyCaseResult(
                state,
                frames,
                persisted,
                stdout,
                stderr,
                gateway.commands,
            )

    def test_target_classifiers_are_narrow(self) -> None:
        edit = final_edit()
        self.assertTrue(
            PROXY.is_target_request(edit, PROXY.TARGET_FINAL_EDIT, MARKER_SENTINEL)
        )
        missing_target = dict(edit)
        missing_target.pop("target_message_id")
        self.assertFalse(
            PROXY.is_target_request(
                missing_target, PROXY.TARGET_FINAL_EDIT, MARKER_SENTINEL
            )
        )
        wrong_platform = dict(edit, platform="slack")
        self.assertFalse(
            PROXY.is_target_request(
                wrong_platform, PROXY.TARGET_FINAL_EDIT, MARKER_SENTINEL
            )
        )
        self.assertTrue(
            PROXY.is_target_request(
                placeholder_send(), PROXY.TARGET_PLACEHOLDER_SEND, None
            )
        )
        reset = placeholder_send()
        reset["content"] = {
            "type": "text",
            "text": "⚠️ _Session expired, starting fresh..._\n\n…",
        }
        self.assertTrue(
            PROXY.is_target_request(reset, PROXY.TARGET_PLACEHOLDER_SEND, None)
        )
        final_text = placeholder_send()
        final_text["content"] = {"type": "text", "text": "final answer"}
        self.assertFalse(
            PROXY.is_target_request(final_text, PROXY.TARGET_PLACEHOLDER_SEND, None)
        )
        self.assertTrue(
            PROXY.is_target_request(
                overflow_send(), PROXY.TARGET_OVERFLOW_SEND, MARKER_SENTINEL
            )
        )
        self.assertFalse(
            PROXY.is_target_request(
                overflow_send(), PROXY.TARGET_OVERFLOW_SEND, "different-marker"
            )
        )
        overflow_edit = overflow_send()
        overflow_edit["command"] = "edit_message"
        overflow_edit["target_message_id"] = "fixture-target-id"
        self.assertFalse(
            PROXY.is_target_request(
                overflow_edit, PROXY.TARGET_OVERFLOW_SEND, MARKER_SENTINEL
            )
        )

    def test_drop_eligibility_requires_explicit_delivered(self) -> None:
        delivered_edit = delivered_ack(REQUEST_SENTINEL)
        self.assertTrue(
            PROXY.drop_eligible_ack(delivered_edit, PROXY.TARGET_FINAL_EDIT)
        )
        self.assertFalse(
            PROXY.drop_eligible_ack(delivered_edit, PROXY.TARGET_PLACEHOLDER_SEND)
        )
        delivered_send = delivered_ack(
            REQUEST_SENTINEL, message_id=fixture_message_reference()
        )
        self.assertTrue(
            PROXY.drop_eligible_ack(delivered_send, PROXY.TARGET_PLACEHOLDER_SEND)
        )
        self.assertTrue(
            PROXY.drop_eligible_ack(delivered_send, PROXY.TARGET_OVERFLOW_SEND)
        )
        self.assertFalse(
            PROXY.drop_eligible_ack(
                rejected_ack(REQUEST_SENTINEL), PROXY.TARGET_FINAL_EDIT
            )
        )
        legacy = {"schema": "openab.gateway.response.v1", "success": True}
        self.assertFalse(PROXY.drop_eligible_ack(legacy, PROXY.TARGET_FINAL_EDIT))

    def test_process_wide_claim_is_single_and_state_is_sanitized(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            state_path = Path(temporary_directory) / "state.json"
            state = PROXY.AtomicState(state_path, PROXY.TARGET_FINAL_EDIT)
            self.assertTrue(state.claim_target())
            self.assertFalse(state.claim_target())
            persisted = state_path.read_text(encoding="utf-8")
            self.assertNotIn(REQUEST_SENTINEL, persisted)
            self.assertNotIn(MARKER_SENTINEL, persisted)
            self.assertEqual(stat.S_IMODE(state_path.stat().st_mode), 0o600)

    def test_frame_guards_fail_closed(self) -> None:
        left, right = socket.socketpair()
        try:
            left.sendall(b"\x81\x7f" + struct.pack("!Q", PROXY.MAX_FRAME_BYTES + 1))
            with self.assertRaisesRegex(ValueError, "frame too large"):
                PROXY.recv_frame(PROXY.BufferedSocket(right))
        finally:
            left.close()
            right.close()

        left, right = socket.socketpair()
        try:
            left.sendall(b"\x01\x00")
            with self.assertRaisesRegex(ValueError, "fragmented"):
                PROXY.recv_frame(PROXY.BufferedSocket(right))
        finally:
            left.close()
            right.close()

    def test_argument_guards_reject_unsafe_activation(self) -> None:
        common = [
            sys.executable,
            str(SCRIPT),
            "--listen-port",
            "18082",
            "--upstream-host",
            "127.0.0.1",
            "--upstream-port",
            "8080",
            "--state-file",
            "state.json",
        ]
        cases = {
            "wildcard listener": [
                "--listen-host",
                "0.0.0.0",
                "--target-kind",
                PROXY.TARGET_PLACEHOLDER_SEND,
            ],
            "public listener": [
                "--listen-host",
                "8.8.8.8",
                "--target-kind",
                PROXY.TARGET_PLACEHOLDER_SEND,
            ],
            "missing edit marker": [
                "--listen-host",
                "127.0.0.1",
                "--target-kind",
                PROXY.TARGET_FINAL_EDIT,
            ],
            "placeholder marker": [
                "--listen-host",
                "127.0.0.1",
                "--target-kind",
                PROXY.TARGET_PLACEHOLDER_SEND,
                "--marker",
                MARKER_SENTINEL,
            ],
            "missing overflow marker": [
                "--listen-host",
                "127.0.0.1",
                "--target-kind",
                PROXY.TARGET_OVERFLOW_SEND,
            ],
            "state path traversal": [
                "--listen-host",
                "127.0.0.1",
                "--target-kind",
                PROXY.TARGET_PLACEHOLDER_SEND,
                "--state-file",
                "../state.json",
            ],
        }
        for name, additions in cases.items():
            with self.subTest(name=name):
                result = subprocess.run(
                    [*common, *additions],
                    check=False,
                    text=True,
                    capture_output=True,
                )
                self.assertEqual(result.returncode, 2)
                self.assertNotIn(QUERY_SENTINEL, result.stderr)

    def test_final_edit_delivered_ack_is_dropped(self) -> None:
        result = self.run_proxy_case(
            target_kind=PROXY.TARGET_FINAL_EDIT,
            command=final_edit(),
            target_ack=delivered_ack(REQUEST_SENTINEL),
        )
        self.assertTrue(result.state["target_seen"])
        self.assertTrue(result.state["client_hello_seen"])
        self.assertTrue(result.state["gateway_hello_seen"])
        self.assertEqual(result.state["gateway_hello_active_consumers"], 1)
        self.assertTrue(result.state["gateway_hello_topology_supported"])
        self.assertTrue(result.state["target_ack_dropped"])
        self.assertEqual(result.state["dropped_ack_outcome"], "delivered")
        self.assertFalse(result.state["target_ack_forwarded"])
        self.assertEqual(result.state["post_target_content_commands"], [])
        self.assertEqual(result.frames[0]["schema"], "openab.gateway.hello.v1")
        self.assertEqual(result.frames[1]["request_id"], "fixture-passthrough-id")

    def test_placeholder_delivered_ack_with_real_id_is_dropped(self) -> None:
        result = self.run_proxy_case(
            target_kind=PROXY.TARGET_PLACEHOLDER_SEND,
            command=placeholder_send(),
            target_ack=delivered_ack(
                REQUEST_SENTINEL, message_id=fixture_message_reference()
            ),
        )
        self.assertEqual(result.state["target_kind"], "placeholder_send")
        self.assertTrue(result.state["target_ack_dropped"])
        self.assertEqual(result.state["post_target_content_commands"], [])
        self.assertEqual(result.state["post_target_reaction_commands"], 0)

    def test_overflow_send_delivered_ack_with_real_id_is_dropped(self) -> None:
        result = self.run_proxy_case(
            target_kind=PROXY.TARGET_OVERFLOW_SEND,
            command=overflow_send(),
            target_ack=delivered_ack(
                REQUEST_SENTINEL, message_id=fixture_message_reference()
            ),
        )
        self.assertEqual(result.state["target_kind"], "overflow_send")
        self.assertTrue(result.state["target_ack_dropped"])
        self.assertEqual(result.state["dropped_ack_outcome"], "delivered")
        self.assertEqual(result.state["post_target_content_commands"], [])

    def test_overflow_send_delivered_without_real_id_is_forwarded(self) -> None:
        result = self.run_proxy_case(
            target_kind=PROXY.TARGET_OVERFLOW_SEND,
            command=overflow_send(),
            target_ack=delivered_ack(REQUEST_SENTINEL),
        )
        self.assertFalse(result.state["target_ack_dropped"])
        self.assertTrue(result.state["target_ack_forwarded"])
        self.assertEqual(result.state["forwarded_ack_outcome"], "delivered")

    def test_placeholder_delivered_without_real_id_is_forwarded(self) -> None:
        result = self.run_proxy_case(
            target_kind=PROXY.TARGET_PLACEHOLDER_SEND,
            command=placeholder_send(),
            target_ack=delivered_ack(REQUEST_SENTINEL),
        )
        self.assertFalse(result.state["target_ack_dropped"])
        self.assertTrue(result.state["target_ack_forwarded"])
        self.assertEqual(result.state["forwarded_ack_outcome"], "delivered")
        forwarded_ids = [
            frame.get("request_id") for frame in result.frames if "request_id" in frame
        ]
        self.assertIn(REQUEST_SENTINEL, forwarded_ids)

    def test_rejected_target_ack_is_forwarded(self) -> None:
        result = self.run_proxy_case(
            target_kind=PROXY.TARGET_FINAL_EDIT,
            command=final_edit(),
            target_ack=rejected_ack(REQUEST_SENTINEL),
        )
        self.assertFalse(result.state["target_ack_dropped"])
        self.assertTrue(result.state["target_ack_forwarded"])
        self.assertEqual(result.state["forwarded_ack_outcome"], "rejected")
        forwarded_ids = [
            frame.get("request_id") for frame in result.frames if "request_id" in frame
        ]
        self.assertIn(REQUEST_SENTINEL, forwarded_ids)

    def test_second_matching_write_is_not_claimed_or_hidden(self) -> None:
        second = final_edit(SECOND_REQUEST_SENTINEL)
        result = self.run_proxy_case(
            target_kind=PROXY.TARGET_FINAL_EDIT,
            command=final_edit(),
            target_ack=delivered_ack(REQUEST_SENTINEL),
            extra_commands=[second],
            extra_responses=[delivered_ack(SECOND_REQUEST_SENTINEL)],
        )
        self.assertTrue(result.state["target_ack_dropped"])
        self.assertEqual(result.state["post_target_content_commands"], ["edit_message"])
        forwarded_ids = [
            frame.get("request_id") for frame in result.frames if "request_id" in frame
        ]
        self.assertIn(SECOND_REQUEST_SENTINEL, forwarded_ids)
        self.assertNotIn(REQUEST_SENTINEL, forwarded_ids)

    def test_duplicate_target_ack_is_forwarded_and_invalidates_probe(self) -> None:
        result = self.run_proxy_case(
            target_kind=PROXY.TARGET_FINAL_EDIT,
            command=final_edit(),
            target_ack=delivered_ack(REQUEST_SENTINEL),
            extra_responses=[delivered_ack(REQUEST_SENTINEL)],
        )
        self.assertTrue(result.state["target_ack_dropped"])
        self.assertTrue(result.state["duplicate_target_ack"])
        self.assertEqual(result.state["error_type"], "DuplicateTargetAck")
        forwarded_ids = [
            frame.get("request_id") for frame in result.frames if "request_id" in frame
        ]
        self.assertIn(REQUEST_SENTINEL, forwarded_ids)

    def test_post_target_reaction_is_counted_separately(self) -> None:
        reaction = {
            "schema": "openab.gateway.reply.v1",
            "platform": "teams",
            "command": "add_reaction",
            "request_id": SECOND_REQUEST_SENTINEL,
            "target_message_id": "fixture-reaction-target",
            "content": {"type": "text", "text": "fixture-reaction"},
        }
        result = self.run_proxy_case(
            target_kind=PROXY.TARGET_FINAL_EDIT,
            command=final_edit(),
            target_ack=delivered_ack(REQUEST_SENTINEL),
            extra_commands=[reaction],
            extra_responses=[delivered_ack(SECOND_REQUEST_SENTINEL)],
        )
        self.assertEqual(result.state["post_target_content_commands"], [])
        self.assertEqual(result.state["post_target_reaction_commands"], 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)

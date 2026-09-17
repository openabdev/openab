//! Minimal ACP agent stub for `openab-core` dispatcher integration tests.
//!
//! Speaks just enough of the ACP JSON-RPC-over-stdio protocol to drive the real
//! `SessionPool` -> `AcpConnection` lifecycle to completion from
//! `AdapterRouter::stream_prompt_blocks`:
//!
//! 1. `initialize` -> advertises a stub agent (no `loadSession` capability).
//! 2. `session/new` -> returns a fixed `sessionId`.
//! 3. `session/prompt` -> emits one `agent_message_chunk` notification with the
//!    text `"Hello, world!"`, then the final id-bearing response with
//!    `stopReason: "end_turn"`.
//!
//! It deliberately does NOT model tool calls; that keeps the stub deterministic
//! and focused on the dispatcher / router / session-streaming seam that issue
//! #1527 asks to cover. Id-bearing requests it does not model get a JSON-RPC
//! `-32601` error rather than silence — a dropped request would otherwise leave
//! the caller parked until `send_request`'s 30s timeout. Notifications (no id,
//! e.g. `session/cancel`) still get no reply, matching the spec.

use std::io::{self, BufRead, Write};

use serde_json::json;

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l.trim().to_string(),
            Err(_) => break,
        };
        if line.is_empty() {
            continue;
        }

        let msg = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = msg.get("id").and_then(|v| v.as_u64());
        let method = msg.get("method").and_then(|v| v.as_str());

        match method {
            Some("initialize") => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": id.unwrap_or(0),
                    "result": {
                        "protocolVersion": 1,
                        "agentInfo": { "name": "oab-acp-stub", "version": "1.0.0" },
                        "agentCapabilities": {}
                    }
                });
                emit(&mut out, &resp);
            }
            Some("session/new") => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": id.unwrap_or(0),
                    "result": { "sessionId": "sess-1" }
                });
                emit(&mut out, &resp);
            }
            Some("session/prompt") => {
                // Stream a single text chunk, then the final completion response.
                let notif = json!({
                    "jsonrpc": "2.0",
                    "method": "session/updated",
                    "params": {
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": { "text": "Hello, world!" }
                        }
                    }
                });
                emit(&mut out, &notif);

                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": id.unwrap_or(0),
                    "result": {
                        "stopReason": "end_turn",
                        "usage": {
                            "inputTokens": 1,
                            "outputTokens": 5,
                            "totalTokens": 6
                        }
                    }
                });
                emit(&mut out, &resp);
            }
            // Fire-and-forget notifications with no id (e.g. session/cancel) need
            // no response. An id-bearing request we don't model gets a real
            // JSON-RPC error — silence would hang the caller for the full
            // send_request timeout (30s) and turn a stub gap into a CI slowdown.
            _ => {
                if let Some(req_id) = id {
                    let resp = json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "error": {
                            "code": -32601,
                            "message": format!("oab_acp_stub_agent does not model {}", method.unwrap_or("<none>"))
                        }
                    });
                    emit(&mut out, &resp);
                }
            }
        }
    }
}

fn emit<W: Write>(out: &mut W, value: &serde_json::Value) {
    if let Ok(s) = serde_json::to_string(value) {
        let _ = writeln!(out, "{s}");
        let _ = out.flush();
    }
}

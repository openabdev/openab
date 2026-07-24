//! `HostBridge` — duplex agent→host request channel shared by the MCP
//! runtime (elicitation routing) and the ACP server loop. Moved here from
//! `openab-agent/src/acp.rs` so the broker-hosted OAB MCP Facade can link
//! the runtime without pulling the whole agent (OAB MCP Adapter ADR §6.2).

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, Value>>>>>;

/// Duplex channel from the MCP client layer back into the ACP loop.
///
/// The ACP transport (`AcpServer::run`) is otherwise half-duplex: it answers
/// inbound host→agent requests and emits fire-and-forget `session/update`
/// notifications. `HostBridge` adds the missing agent→host *request/response*
/// direction so an MCP `ClientHandler` running on an rmcp task can ask the
/// host a question (e.g. elicitation form) and await a structured reply.
///
/// All outbound bytes funnel through `writer` (a single stdout-owning drain
/// task) to preserve the one-writer invariant; `pending` correlates each
/// outbound id with its awaiting `oneshot`; `next_id` mints monotonic ids.
#[derive(Clone, Debug)]
pub struct HostBridge {
    writer: mpsc::UnboundedSender<String>,
    pending: PendingMap,
    next_id: Arc<AtomicU64>,
}

impl HostBridge {
    pub fn new(writer: mpsc::UnboundedSender<String>) -> Self {
        Self {
            writer,
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Send an outbound agent→host request and await the host's response.
    /// Returns `Ok(result)` / `Err(error)` mirroring JSON-RPC. Returns `Err`
    /// (rather than blocking forever) when no host is listening or the channel
    /// is closed, so callers can degrade gracefully (e.g. auto-decline).
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply_tx, reply_rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("host bridge pending map poisoned")
            .insert(id, reply_tx);

        let line = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
        .to_string();

        if self.writer.send(line).is_err() {
            self.pending
                .lock()
                .expect("host bridge pending map poisoned")
                .remove(&id);
            return Err(json!({ "code": -32603, "message": "host channel closed" }));
        }

        match reply_rx.await {
            Ok(outcome) => outcome,
            Err(_) => Err(json!({ "code": -32603, "message": "host reply dropped" })),
        }
    }

    /// If `line` is an inbound JSON-RPC *response* to one of our outbound
    /// requests, resolve the matching pending `oneshot` and return `true`.
    /// Returns `false` for anything else (inbound requests, notifications,
    /// unknown / already-completed ids) so the caller falls through to the
    /// normal request-dispatch path.
    pub fn try_resolve_response(&self, line: &str) -> bool {
        let val: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return false,
        };
        // A response carries an id + (result | error) and NO method. A request
        // or notification has a method — leave those for the dispatch loop.
        if val.get("method").is_some() {
            return false;
        }
        let Some(id) = val.get("id").and_then(|v| v.as_u64()) else {
            return false;
        };
        let Some(reply_tx) = self
            .pending
            .lock()
            .expect("host bridge pending map poisoned")
            .remove(&id)
        else {
            return false;
        };
        let outcome = if let Some(err) = val.get("error") {
            Err(err.clone())
        } else {
            Ok(val.get("result").cloned().unwrap_or(Value::Null))
        };
        let _ = reply_tx.send(outcome);
        true
    }
}

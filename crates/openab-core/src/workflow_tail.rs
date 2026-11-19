//! Relay of late Kiro workflow output.
//!
//! With `[agent].session_new_meta` enabling Kiro workflows, `session/prompt`
//! can return before a workflow finishes. The workflow's summary then arrives
//! later as unsolicited `agent_message_chunk` frames of the parent session
//! (no request id). The adapter keeps the notify receiver after the reply is
//! delivered and hands it to [`run_tail`], which buffers that text and posts
//! it as a follow-up message.
//!
//! The tail holds no connection lock. The next prompt on the same session
//! replaces the subscriber, which closes the receiver and ends the tail.

use crate::acp::protocol::JsonRpcMessage;
use crate::acp::{
    classify_notification, classify_workflow_signal, is_foreign_session_update, AcpEvent,
    WorkflowSignal,
};
use std::collections::HashSet;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::oneshot;
use tokio::time::{sleep_until, timeout_at, Instant};

/// Bounds for the late-output relay (`[pool] workflow_tail_*`).
#[derive(Debug, Clone, Copy)]
pub struct WorkflowRelayConfig {
    /// Flush buffered text after this long without relevant frames.
    pub tail_quiet: Duration,
    /// Hard cap from the prompt response. Zero disables the relay (the
    /// session-id filter stays on).
    pub tail_max: Duration,
}

/// Workflow bookkeeping for one turn, fed with every frame of the turn.
#[derive(Debug, Default)]
pub struct TurnWorkflowState {
    open: HashSet<String>,
    seen: bool,
}

impl TurnWorkflowState {
    pub fn observe(&mut self, msg: &JsonRpcMessage, parent_session_id: &str) {
        match classify_workflow_signal(msg, parent_session_id) {
            Some(WorkflowSignal::RunStart { workflow_id }) => {
                self.seen = true;
                self.open.insert(workflow_id);
            }
            Some(WorkflowSignal::RunComplete { workflow_id }) => {
                self.open.remove(&workflow_id);
            }
            None => {}
        }
    }

    /// A workflow started for this session during the turn.
    pub fn armed(&self) -> bool {
        self.seen
    }

    pub fn into_open(self) -> HashSet<String> {
        self.open
    }
}

/// Most frames queued between the connection reader and the tail. A full queue
/// drops the newest frame and marks the next flush as truncated.
pub const RELAY_QUEUE_CAPACITY: usize = 1024;

/// Producer side of the relay queue. `send` never waits.
#[derive(Debug, Clone)]
pub struct RelaySender {
    tx: mpsc::Sender<JsonRpcMessage>,
    overflow: Arc<AtomicBool>,
}

/// A frame was not queued: the queue was full or the tail has ended.
#[derive(Debug)]
pub struct RelayDropped;

impl RelaySender {
    pub fn send(&self, msg: JsonRpcMessage) -> Result<(), RelayDropped> {
        match self.tx.try_send(msg) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.overflow.store(true, Ordering::Relaxed);
                Err(RelayDropped)
            }
            Err(TrySendError::Closed(_)) => Err(RelayDropped),
        }
    }
}

/// Consumer side of the relay queue, read by [`run_tail`].
#[derive(Debug)]
pub struct RelayReceiver {
    rx: mpsc::Receiver<JsonRpcMessage>,
    overflow: Arc<AtomicBool>,
}

impl RelayReceiver {
    pub async fn recv(&mut self) -> Option<JsonRpcMessage> {
        self.rx.recv().await
    }

    pub fn try_recv(&mut self) -> Option<JsonRpcMessage> {
        self.rx.try_recv().ok()
    }

    /// True once if a frame was dropped on a full queue since the last call.
    fn take_overflow(&self) -> bool {
        self.overflow.swap(false, Ordering::Relaxed)
    }
}

/// A bounded relay queue of [`RELAY_QUEUE_CAPACITY`] frames.
pub fn relay_channel() -> (RelaySender, RelayReceiver) {
    let (tx, rx) = mpsc::channel(RELAY_QUEUE_CAPACITY);
    let overflow = Arc::new(AtomicBool::new(false));
    (
        RelaySender {
            tx,
            overflow: overflow.clone(),
        },
        RelayReceiver { rx, overflow },
    )
}

/// Most text the tail buffers between flushes. Further text is dropped and a
/// truncation note is appended to the flushed message.
pub const MAX_BUFFER_BYTES: usize = 256 * 1024;

/// Bound on one `deliver` call, so a stalled platform call cannot keep the
/// tail (and its deadline checks) blocked.
pub const DELIVER_TIMEOUT: Duration = Duration::from_secs(30);

/// Most platform messages one late-output flush may post. When the text needs
/// more, the last message is [`TRUNCATED_NOTE`] and the rest is not posted.
pub const MAX_FLUSH_MESSAGES: usize = 16;

/// Posted in place of the chunks that do not fit into [`MAX_FLUSH_MESSAGES`].
pub const TRUNCATED_NOTE: &str = "…(output truncated: this flush was cut at 16 platform messages and the rest of this flush was not sent; later output is still relayed)";

/// Apply the per-flush message cap to already platform-split `chunks`. The
/// note takes the last slot, so it is always posted when anything is dropped.
pub fn limit_flush_messages(mut chunks: Vec<String>, max: usize, note: &str) -> Vec<String> {
    if chunks.len() <= max {
        return chunks;
    }
    chunks.truncate(max.saturating_sub(1));
    chunks.push(note.to_string());
    chunks
}

/// Read late frames from `rx` and hand text to `deliver`.
///
/// `started` is when the prompt response arrived: the hard cap is measured
/// from it, not from when this task starts. `gate` resolves once the original
/// reply has been delivered; the tail starts draining `rx` immediately (so the
/// queue does not grow while the reply is finalized) but posts nothing before
/// the gate opens. A dropped gate counts as open.
///
/// Quiet time only decides when buffered text is posted. The tail does not end
/// on quiet, so output that arrives after a gap is still delivered. It ends
/// when the channel closes (flushing what is buffered) or at the hard cap
/// (flushing, and reporting through `deliver` if a run is still open). Any
/// parent-session `session/update` restarts the quiet timer. Buffered text is
/// limited to [`MAX_BUFFER_BYTES`] and each `deliver` to [`DELIVER_TIMEOUT`].
/// Frames dropped on a full queue ([`RELAY_QUEUE_CAPACITY`]) mark the flushed
/// text as truncated.
pub async fn run_tail<F, Fut>(
    mut rx: RelayReceiver,
    parent_session_id: String,
    open: HashSet<String>,
    cfg: WorkflowRelayConfig,
    started: Instant,
    gate: oneshot::Receiver<()>,
    mut deliver: F,
) where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = ()>,
{
    let mut gate = gate;
    let mut gated = true;
    let hard = started + cfg.tail_max;
    let mut st = TailState {
        parent: parent_session_id,
        open,
        buf: String::new(),
        truncated: false,
        last_event: Instant::now(),
        closed: false,
        ending: false,
    };
    loop {
        let quiet_applies = !st.buf.is_empty() || st.open.is_empty();
        let deadline = if quiet_applies {
            (st.last_event + cfg.tail_quiet).min(hard)
        } else {
            hard
        };
        tokio::select! {
            _ = &mut gate, if gated => {
                gated = false;
            }
            msg = rx.recv() => {
                let Some(msg) = msg else {
                    if gated {
                        // Wait for the original reply, but not past the cap.
                        if timeout_at(hard, &mut gate).await.is_err() {
                            return;
                        }
                    }
                    flush(&mut st, &mut rx, &mut deliver).await;
                    return;
                };
                st.ingest(msg);
            }
            _ = sleep_until(deadline) => {
                let at_cap = Instant::now() >= hard;
                if gated && !at_cap {
                    // Reply still being delivered: keep buffering.
                    st.last_event = Instant::now();
                    continue;
                }
                if gated {
                    // Cap reached before the reply was delivered; drop.
                    return;
                }
                flush(&mut st, &mut rx, &mut deliver).await;
                if at_cap || Instant::now() >= hard {
                    // Frames drained while that delivery was pending are still
                    // buffered: one bounded final flush (frames arriving during
                    // it are dropped, the tail ends here).
                    st.ending = true;
                    flush(&mut st, &mut rx, &mut deliver).await;
                    if !st.open.is_empty() {
                        send(&mut st, &mut rx, &mut deliver, format!(
                            "⚠ workflow still running after {}s; further output is not relayed",
                            cfg.tail_max.as_secs()
                        ))
                        .await;
                    }
                    return;
                }
                st.last_event = Instant::now();
            }
        }
    }
}

/// Mutable bookkeeping of one tail.
struct TailState {
    parent: String,
    open: HashSet<String>,
    buf: String,
    truncated: bool,
    last_event: Instant,
    /// The sender side closed while a delivery was pending.
    closed: bool,
    /// The final delivery is running: queued frames are read and dropped so
    /// the queue keeps draining until the tail returns.
    ending: bool,
}

impl TailState {
    fn ingest(&mut self, msg: JsonRpcMessage) {
        if is_foreign_session_update(&msg, &self.parent) {
            return;
        }
        if msg.id.is_none() && msg.method.as_deref() == Some("session/update") {
            // Thinking and tool frames are not relayed, but they count as
            // activity so a pause inside a summary does not split it.
            self.last_event = Instant::now();
        }
        match classify_workflow_signal(&msg, &self.parent) {
            Some(WorkflowSignal::RunStart { workflow_id }) => {
                self.open.insert(workflow_id);
                self.last_event = Instant::now();
                return;
            }
            Some(WorkflowSignal::RunComplete { workflow_id }) => {
                self.open.remove(&workflow_id);
                self.last_event = Instant::now();
                return;
            }
            None => {}
        }
        if msg.id.is_none() {
            if let Some(AcpEvent::Text(t)) = classify_notification(&msg) {
                // Once text is cut, later text is dropped too, so the flushed
                // message stays a contiguous prefix before the truncation note.
                if !self.truncated && self.buf.len() + t.len() <= MAX_BUFFER_BYTES {
                    self.buf.push_str(&t);
                } else {
                    self.truncated = true;
                }
                self.last_event = Instant::now();
            }
        }
    }
}

/// Run one delivery (bounded by [`DELIVER_TIMEOUT`]) while still draining
/// `rx` into the capped buffer, so the notification queue cannot grow while
/// the platform call is pending.
async fn send<F, Fut>(st: &mut TailState, rx: &mut RelayReceiver, deliver: &mut F, text: String)
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = ()>,
{
    let fut = tokio::time::timeout(DELIVER_TIMEOUT, deliver(text));
    tokio::pin!(fut);
    loop {
        tokio::select! {
            _ = &mut fut => return,
            msg = rx.recv(), if !st.closed => match msg {
                Some(m) => {
                    if !st.ending {
                        st.ingest(m);
                    }
                }
                None => st.closed = true,
            },
        }
    }
}

async fn flush<F, Fut>(st: &mut TailState, rx: &mut RelayReceiver, deliver: &mut F)
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = ()>,
{
    let mut text = std::mem::take(&mut st.buf);
    let dropped = rx.take_overflow();
    if std::mem::take(&mut st.truncated) || dropped {
        text.push_str("\n\n…(output truncated)");
    }
    if !text.trim().is_empty() {
        send(st, rx, deliver, text).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tokio::sync::oneshot;

    const PARENT: &str = "parent";

    fn cfg() -> WorkflowRelayConfig {
        WorkflowRelayConfig {
            tail_quiet: Duration::from_secs(20),
            tail_max: Duration::from_secs(900),
        }
    }

    fn frame(v: serde_json::Value) -> JsonRpcMessage {
        serde_json::from_value(v).unwrap()
    }

    fn run_start(parent: &str) -> JsonRpcMessage {
        frame(json!({"method": "_kiro/workflow/run_start",
            "params": {"workflowId": "wf_1", "parentSessionId": parent}}))
    }

    fn run_complete() -> JsonRpcMessage {
        frame(json!({"method": "_kiro/workflow/run_complete",
            "params": {"workflowId": "wf_1", "status": "completed"}}))
    }

    fn chunk(session: &str, text: &str) -> JsonRpcMessage {
        frame(
            json!({"method": "session/update", "params": {"sessionId": session,
            "update": {"sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": text}}}}),
        )
    }

    fn collector() -> (
        Arc<Mutex<Vec<String>>>,
        impl FnMut(String) -> std::future::Ready<()>,
    ) {
        let out = Arc::new(Mutex::new(Vec::new()));
        let sink = out.clone();
        (out, move |t| {
            sink.lock().unwrap().push(t);
            std::future::ready(())
        })
    }

    fn open_gate() -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        tx.send(()).unwrap();
        rx
    }

    fn open_set(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn late_parent_text_is_delivered_after_run_complete_and_quiet() {
        let (tx, rx) = relay_channel();
        let (out, deliver) = collector();
        tx.send(run_complete()).unwrap();
        tx.send(chunk(PARENT, "all ")).unwrap();
        tx.send(chunk(PARENT, "done")).unwrap();
        run_tail(
            rx,
            PARENT.into(),
            open_set(&["wf_1"]),
            cfg(),
            Instant::now(),
            open_gate(),
            deliver,
        )
        .await;
        assert_eq!(*out.lock().unwrap(), vec!["all done".to_string()]);
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn step_session_text_is_dropped() {
        let (tx, rx) = relay_channel();
        let (out, deliver) = collector();
        tx.send(run_complete()).unwrap();
        tx.send(chunk("step-1", "internal")).unwrap();
        tx.send(chunk(PARENT, "summary")).unwrap();
        run_tail(
            rx,
            PARENT.into(),
            open_set(&["wf_1"]),
            cfg(),
            Instant::now(),
            open_gate(),
            deliver,
        )
        .await;
        assert_eq!(*out.lock().unwrap(), vec!["summary".to_string()]);
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn replaced_subscriber_flushes_buffer_and_ends() {
        let (tx, rx) = relay_channel();
        let (out, deliver) = collector();
        tx.send(chunk(PARENT, "partial")).unwrap();
        drop(tx);
        run_tail(
            rx,
            PARENT.into(),
            open_set(&["wf_1"]),
            cfg(),
            Instant::now(),
            open_gate(),
            deliver,
        )
        .await;
        assert_eq!(*out.lock().unwrap(), vec!["partial".to_string()]);
    }

    #[tokio::test(start_paused = true)]
    async fn run_that_never_completes_reports_at_the_cap() {
        let (tx, rx) = relay_channel();
        let (out, deliver) = collector();
        let started = Instant::now();
        run_tail(
            rx,
            PARENT.into(),
            open_set(&["wf_1"]),
            cfg(),
            Instant::now(),
            open_gate(),
            deliver,
        )
        .await;
        assert_eq!(started.elapsed(), Duration::from_secs(900));
        let got = out.lock().unwrap().clone();
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("still running after 900s"), "{got:?}");
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn text_during_open_run_is_flushed_after_quiet_and_tail_continues() {
        let (tx, rx) = relay_channel();
        let (out, deliver) = collector();
        let handle = tokio::spawn(run_tail(
            rx,
            PARENT.into(),
            open_set(&["wf_1"]),
            cfg(),
            Instant::now(),
            open_gate(),
            deliver,
        ));
        tx.send(chunk(PARENT, "first")).unwrap();
        tokio::time::sleep(Duration::from_secs(25)).await;
        assert_eq!(*out.lock().unwrap(), vec!["first".to_string()]);
        assert!(!handle.is_finished(), "run is still open");
        tx.send(run_complete()).unwrap();
        tx.send(chunk(PARENT, "second")).unwrap();
        handle.await.unwrap();
        assert_eq!(
            *out.lock().unwrap(),
            vec!["first".to_string(), "second".to_string()]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn idle_tail_without_open_run_ends_at_cap_with_no_delivery() {
        let (tx, rx) = relay_channel();
        let (out, deliver) = collector();
        let started = Instant::now();
        run_tail(
            rx,
            PARENT.into(),
            HashSet::new(),
            cfg(),
            Instant::now(),
            open_gate(),
            deliver,
        )
        .await;
        assert_eq!(started.elapsed(), Duration::from_secs(900));
        assert!(out.lock().unwrap().is_empty());
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn summary_after_a_quiet_gap_is_delivered() {
        let (tx, rx) = relay_channel();
        let (out, deliver) = collector();
        let handle = tokio::spawn(run_tail(
            rx,
            PARENT.into(),
            HashSet::new(),
            cfg(),
            Instant::now(),
            open_gate(),
            deliver,
        ));
        tx.send(run_complete()).unwrap();
        tokio::time::sleep(Duration::from_secs(25)).await;
        tx.send(chunk(PARENT, "summary")).unwrap();
        tokio::time::sleep(Duration::from_secs(25)).await;
        assert_eq!(*out.lock().unwrap(), vec!["summary".to_string()]);
        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn thought_and_tool_frames_keep_a_summary_in_one_message() {
        let (tx, rx) = relay_channel();
        let (out, deliver) = collector();
        let handle = tokio::spawn(run_tail(
            rx,
            PARENT.into(),
            open_set(&["wf_1"]),
            cfg(),
            Instant::now(),
            open_gate(),
            deliver,
        ));
        tx.send(chunk(PARENT, "A")).unwrap();
        // Thinking and tool calls arrive every 15 s, each inside the quiet
        // window, so "A" and "B" stay in one message.
        tokio::time::sleep(Duration::from_secs(15)).await;
        tx.send(frame(json!({"method": "session/update", "params": {
            "sessionId": PARENT,
            "update": {"sessionUpdate": "agent_thought_chunk",
                "content": {"type": "text", "text": "thinking"}}}})))
            .unwrap();
        tokio::time::sleep(Duration::from_secs(15)).await;
        tx.send(frame(json!({"method": "session/update", "params": {
            "sessionId": PARENT,
            "update": {"sessionUpdate": "tool_call", "toolCallId": "t1",
                "title": "read"}}})))
            .unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
        tx.send(chunk(PARENT, "B")).unwrap();
        tokio::time::sleep(Duration::from_secs(40)).await;
        assert_eq!(*out.lock().unwrap(), vec!["AB".to_string()]);
        drop(tx);
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn closed_channel_with_empty_buffer_ends_without_delivery() {
        let (tx, rx) = relay_channel();
        drop(tx);
        let (out, deliver) = collector();
        run_tail(
            rx,
            PARENT.into(),
            open_set(&["wf_1"]),
            cfg(),
            Instant::now(),
            open_gate(),
            deliver,
        )
        .await;
        assert!(out.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn buffered_text_is_capped_and_marked_truncated() {
        let (tx, rx) = relay_channel();
        let (out, deliver) = collector();
        let big = "x".repeat(MAX_BUFFER_BYTES / 2 + 1);
        tx.send(chunk(PARENT, &big)).unwrap();
        tx.send(chunk(PARENT, &big)).unwrap();
        tx.send(chunk(PARENT, &big)).unwrap();
        drop(tx);
        run_tail(
            rx,
            PARENT.into(),
            open_set(&["wf_1"]),
            cfg(),
            Instant::now(),
            open_gate(),
            deliver,
        )
        .await;
        let got = out.lock().unwrap().clone();
        assert_eq!(got.len(), 1);
        assert!(got[0].len() <= MAX_BUFFER_BYTES + 64, "{}", got[0].len());
        assert!(got[0].ends_with("…(output truncated)"));
    }

    #[tokio::test(start_paused = true)]
    async fn text_after_a_cut_is_not_appended_out_of_order() {
        let (tx, rx) = relay_channel();
        let (out, deliver) = collector();
        tx.send(chunk(PARENT, &"a".repeat(MAX_BUFFER_BYTES - 10)))
            .unwrap();
        // Does not fit: the buffer is cut here.
        tx.send(chunk(PARENT, &"b".repeat(100))).unwrap();
        // Would fit in the 10 bytes left, but must not follow the cut.
        tx.send(chunk(PARENT, "ccccc")).unwrap();
        drop(tx);
        run_tail(
            rx,
            PARENT.into(),
            open_set(&["wf_1"]),
            cfg(),
            Instant::now(),
            open_gate(),
            deliver,
        )
        .await;
        assert_eq!(
            *out.lock().unwrap(),
            vec![format!(
                "{}\n\n…(output truncated)",
                "a".repeat(MAX_BUFFER_BYTES - 10)
            )]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn hard_cap_counts_from_the_prompt_response() {
        let (tx, rx) = relay_channel();
        let (out, deliver) = collector();
        // The prompt response arrived 800 s ago (original reply was slow).
        let started = Instant::now();
        tokio::time::advance(Duration::from_secs(800)).await;
        let t0 = Instant::now();
        run_tail(
            rx,
            PARENT.into(),
            open_set(&["wf_1"]),
            cfg(),
            started,
            open_gate(),
            deliver,
        )
        .await;
        assert_eq!(t0.elapsed(), Duration::from_secs(100));
        assert!(out.lock().unwrap()[0].contains("still running"));
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_posted_before_the_original_reply_is_delivered() {
        let (tx, rx) = relay_channel();
        let (gate_tx, gate_rx) = oneshot::channel();
        let (out, deliver) = collector();
        let handle = tokio::spawn(run_tail(
            rx,
            PARENT.into(),
            open_set(&["wf_1"]),
            cfg(),
            Instant::now(),
            gate_rx,
            deliver,
        ));
        tx.send(run_complete()).unwrap();
        tx.send(chunk(PARENT, "late")).unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(out.lock().unwrap().is_empty(), "gate still closed");
        assert!(!handle.is_finished());
        gate_tx.send(()).unwrap();
        handle.await.unwrap();
        assert_eq!(*out.lock().unwrap(), vec!["late".to_string()]);
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_delivery_does_not_hold_the_tail_forever() {
        let (tx, rx) = relay_channel();
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = calls.clone();
        let deliver = move |t: String| {
            let first = {
                let mut g = sink.lock().unwrap();
                g.push(t);
                g.len() == 1
            };
            async move {
                if first {
                    std::future::pending::<()>().await;
                }
            }
        };
        tx.send(run_complete()).unwrap();
        tx.send(chunk(PARENT, "late")).unwrap();
        let handle = tokio::spawn(run_tail(
            rx,
            PARENT.into(),
            open_set(&["wf_1"]),
            cfg(),
            Instant::now(),
            open_gate(),
            deliver,
        ));
        // Quiet (20 s) plus one delivery timeout (30 s); the tail must still
        // post output that arrives after that.
        tokio::time::sleep(Duration::from_secs(60)).await;
        tx.send(chunk(PARENT, "later")).unwrap();
        tokio::time::sleep(Duration::from_secs(40)).await;
        drop(tx);
        handle.await.unwrap();
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["late".to_string(), "later".to_string()]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn frames_sent_during_a_pending_delivery_are_drained_and_capped() {
        let (tx, rx) = relay_channel();
        tx.send(run_complete()).unwrap();
        tx.send(chunk(PARENT, "first")).unwrap();
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = calls.clone();
        let deliver = move |t: String| {
            let first = {
                let mut g = sink.lock().unwrap();
                g.push(t);
                g.len() == 1
            };
            async move {
                if first {
                    // Platform call that hangs until the delivery timeout.
                    std::future::pending::<()>().await;
                }
            }
        };
        let flood = async move {
            // Quiet period (20 s) has passed; the first delivery is pending.
            tokio::time::sleep(Duration::from_secs(25)).await;
            let big = "x".repeat(MAX_BUFFER_BYTES / 2 + 1);
            for _ in 0..50 {
                tx.send(chunk(PARENT, &big)).unwrap();
            }
            // Let the tail drain the queue while the delivery is pending.
            tokio::task::yield_now().await;
            drop(tx);
        };
        let tail = run_tail(
            rx,
            PARENT.into(),
            open_set(&["wf_1"]),
            cfg(),
            Instant::now(),
            open_gate(),
            deliver,
        );
        tokio::join!(tail, flood);
        let got = calls.lock().unwrap().clone();
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0], "first");
        assert!(got[1].len() <= MAX_BUFFER_BYTES + 64, "{}", got[1].len());
        assert!(got[1].ends_with("…(output truncated)"));
    }

    #[tokio::test(start_paused = true)]
    async fn queue_overflow_while_delivery_is_stalled_is_bounded_and_marked() {
        let (tx, rx) = relay_channel();
        tx.send(chunk(PARENT, "first")).unwrap();
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = calls.clone();
        let deliver = move |t: String| {
            let first = {
                let mut g = sink.lock().unwrap();
                g.push(t);
                g.len() == 1
            };
            async move {
                if first {
                    std::future::pending::<()>().await;
                }
            }
        };
        let flood = async move {
            // The first delivery is pending from the 20 s quiet mark.
            tokio::time::sleep(Duration::from_secs(25)).await;
            // One synchronous burst: the tail cannot drain until this task
            // yields, so the queue fills and the surplus is dropped.
            let mut dropped = 0;
            for _ in 0..RELAY_QUEUE_CAPACITY + 500 {
                if tx.send(chunk(PARENT, "x")).is_err() {
                    dropped += 1;
                }
            }
            assert_eq!(dropped, 500);
            tokio::task::yield_now().await;
            drop(tx);
        };
        let tail = run_tail(
            rx,
            PARENT.into(),
            open_set(&["wf_1"]),
            cfg(),
            Instant::now(),
            open_gate(),
            deliver,
        );
        tokio::join!(tail, flood);
        let got = calls.lock().unwrap().clone();
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0], "first");
        assert_eq!(
            got[1],
            format!(
                "{}\n\n…(output truncated)",
                "x".repeat(RELAY_QUEUE_CAPACITY)
            )
        );
    }

    #[tokio::test(start_paused = true)]
    async fn text_buffered_while_a_delivery_crosses_the_hard_cap_is_flushed() {
        let (tx, rx) = relay_channel();
        tx.send(chunk(PARENT, "first")).unwrap();
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = calls.clone();
        let deliver = move |t: String| {
            let first = {
                let mut g = sink.lock().unwrap();
                g.push(t);
                g.len() == 1
            };
            async move {
                if first {
                    // Hangs until DELIVER_TIMEOUT, which ends after the cap.
                    std::future::pending::<()>().await;
                }
            }
        };
        let late = async move {
            // Delivery started at the 20 s quiet mark and is still pending.
            tokio::time::sleep(Duration::from_secs(25)).await;
            tx.send(chunk(PARENT, "late")).unwrap();
            tokio::time::sleep(Duration::from_secs(1000)).await;
            drop(tx);
        };
        let mut c = cfg();
        c.tail_max = Duration::from_secs(40);
        let tail = run_tail(
            rx,
            PARENT.into(),
            HashSet::new(),
            c,
            Instant::now(),
            open_gate(),
            deliver,
        );
        tokio::join!(tail, late);
        let got = calls.lock().unwrap().clone();
        assert_eq!(
            got,
            vec!["first".to_string(), "late".to_string()],
            "{got:?}"
        );
    }

    #[test]
    fn flush_messages_are_capped_with_a_note_in_the_last_slot() {
        let short = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            limit_flush_messages(short.clone(), MAX_FLUSH_MESSAGES, TRUNCATED_NOTE),
            short
        );

        // Exactly the cap: nothing is dropped, so no note.
        let exact: Vec<String> = (0..MAX_FLUSH_MESSAGES).map(|i| i.to_string()).collect();
        assert_eq!(
            limit_flush_messages(exact.clone(), MAX_FLUSH_MESSAGES, TRUNCATED_NOTE),
            exact
        );

        let long: Vec<String> = (0..100).map(|i| i.to_string()).collect();
        let got = limit_flush_messages(long, MAX_FLUSH_MESSAGES, TRUNCATED_NOTE);
        assert_eq!(got.len(), MAX_FLUSH_MESSAGES);
        assert_eq!(
            got[MAX_FLUSH_MESSAGES - 2],
            (MAX_FLUSH_MESSAGES - 2).to_string()
        );
        assert_eq!(got[MAX_FLUSH_MESSAGES - 1], TRUNCATED_NOTE);
        assert_eq!(
            TRUNCATED_NOTE,
            "…(output truncated: this flush was cut at 16 platform messages and the rest of this flush was not sent; later output is still relayed)"
        );
    }

    #[test]
    fn full_buffer_splits_into_at_most_the_cap_on_a_discord_sized_limit() {
        // 256 KiB of text at Discord's 2000-character message limit would be
        // about 130 messages; the cap keeps 15 of them plus the note.
        let text = "line of late output\n".repeat(MAX_BUFFER_BYTES / 20);
        let chunks = crate::format::split_message(&text, 2000);
        assert!(chunks.len() > MAX_FLUSH_MESSAGES);
        let got = limit_flush_messages(chunks, MAX_FLUSH_MESSAGES, TRUNCATED_NOTE);
        assert_eq!(got.len(), MAX_FLUSH_MESSAGES);
        assert_eq!(got.last().unwrap(), TRUNCATED_NOTE);
        assert!(got[..MAX_FLUSH_MESSAGES - 1]
            .iter()
            .all(|c| c.chars().count() <= 2000));
    }

    #[test]
    fn turn_state_arms_only_for_own_session() {
        let mut state = TurnWorkflowState::default();
        state.observe(&run_start("someone-else"), PARENT);
        assert!(!state.armed());
        state.observe(&run_start(PARENT), PARENT);
        assert!(state.armed());
        assert_eq!(state.into_open(), open_set(&["wf_1"]));

        let mut state = TurnWorkflowState::default();
        state.observe(&run_start(PARENT), PARENT);
        state.observe(&run_complete(), PARENT);
        assert!(state.armed());
        assert!(state.into_open().is_empty());
    }
}

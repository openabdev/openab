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
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{sleep_until, Instant};

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

/// Read late frames from `rx` and hand text to `deliver`.
///
/// Ends when the channel closes (flushing what is buffered), when nothing is
/// running and the quiet time has passed, or at `cfg.tail_max` (flushing, and
/// reporting through `deliver` if a run is still open).
pub async fn run_tail<F, Fut>(
    mut rx: UnboundedReceiver<JsonRpcMessage>,
    parent_session_id: String,
    mut open: HashSet<String>,
    cfg: WorkflowRelayConfig,
    mut deliver: F,
) where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = ()>,
{
    let start = Instant::now();
    let hard = start + cfg.tail_max;
    let mut last_event = start;
    let mut buf = String::new();
    loop {
        let quiet_applies = !buf.is_empty() || open.is_empty();
        let deadline = if quiet_applies {
            (last_event + cfg.tail_quiet).min(hard)
        } else {
            hard
        };
        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else {
                    flush(&mut buf, &mut deliver).await;
                    return;
                };
                if is_foreign_session_update(&msg, &parent_session_id) {
                    continue;
                }
                match classify_workflow_signal(&msg, &parent_session_id) {
                    Some(WorkflowSignal::RunStart { workflow_id }) => {
                        open.insert(workflow_id);
                        last_event = Instant::now();
                        continue;
                    }
                    Some(WorkflowSignal::RunComplete { workflow_id }) => {
                        open.remove(&workflow_id);
                        last_event = Instant::now();
                        continue;
                    }
                    None => {}
                }
                if msg.id.is_none() {
                    if let Some(AcpEvent::Text(t)) = classify_notification(&msg) {
                        buf.push_str(&t);
                        last_event = Instant::now();
                    }
                }
            }
            _ = sleep_until(deadline) => {
                flush(&mut buf, &mut deliver).await;
                if Instant::now() >= hard {
                    if !open.is_empty() {
                        deliver(format!(
                            "⚠ workflow still running after {}s; further output is not relayed",
                            cfg.tail_max.as_secs()
                        ))
                        .await;
                    }
                    return;
                }
                last_event = Instant::now();
                if open.is_empty() {
                    return;
                }
            }
        }
    }
}

async fn flush<F, Fut>(buf: &mut String, deliver: &mut F)
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = ()>,
{
    let text = std::mem::take(buf);
    if !text.trim().is_empty() {
        deliver(text).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc::unbounded_channel;

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

    fn open_set(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn late_parent_text_is_delivered_after_run_complete_and_quiet() {
        let (tx, rx) = unbounded_channel();
        let (out, deliver) = collector();
        tx.send(run_complete()).unwrap();
        tx.send(chunk(PARENT, "all ")).unwrap();
        tx.send(chunk(PARENT, "done")).unwrap();
        run_tail(rx, PARENT.into(), open_set(&["wf_1"]), cfg(), deliver).await;
        assert_eq!(*out.lock().unwrap(), vec!["all done".to_string()]);
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn step_session_text_is_dropped() {
        let (tx, rx) = unbounded_channel();
        let (out, deliver) = collector();
        tx.send(run_complete()).unwrap();
        tx.send(chunk("step-1", "internal")).unwrap();
        tx.send(chunk(PARENT, "summary")).unwrap();
        run_tail(rx, PARENT.into(), open_set(&["wf_1"]), cfg(), deliver).await;
        assert_eq!(*out.lock().unwrap(), vec!["summary".to_string()]);
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn replaced_subscriber_flushes_buffer_and_ends() {
        let (tx, rx) = unbounded_channel();
        let (out, deliver) = collector();
        tx.send(chunk(PARENT, "partial")).unwrap();
        drop(tx);
        run_tail(rx, PARENT.into(), open_set(&["wf_1"]), cfg(), deliver).await;
        assert_eq!(*out.lock().unwrap(), vec!["partial".to_string()]);
    }

    #[tokio::test(start_paused = true)]
    async fn run_that_never_completes_reports_at_the_cap() {
        let (tx, rx) = unbounded_channel();
        let (out, deliver) = collector();
        let started = Instant::now();
        run_tail(rx, PARENT.into(), open_set(&["wf_1"]), cfg(), deliver).await;
        assert_eq!(started.elapsed(), Duration::from_secs(900));
        let got = out.lock().unwrap().clone();
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("still running after 900s"), "{got:?}");
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn text_during_open_run_is_flushed_after_quiet_and_tail_continues() {
        let (tx, rx) = unbounded_channel();
        let (out, deliver) = collector();
        let handle = tokio::spawn(run_tail(
            rx,
            PARENT.into(),
            open_set(&["wf_1"]),
            cfg(),
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
    async fn idle_tail_without_open_run_ends_after_quiet_with_no_delivery() {
        let (tx, rx) = unbounded_channel();
        let (out, deliver) = collector();
        let started = Instant::now();
        run_tail(rx, PARENT.into(), HashSet::new(), cfg(), deliver).await;
        assert_eq!(started.elapsed(), Duration::from_secs(20));
        assert!(out.lock().unwrap().is_empty());
        drop(tx);
    }

    #[tokio::test(start_paused = true)]
    async fn closed_channel_with_empty_buffer_ends_without_delivery() {
        let (tx, rx) = unbounded_channel::<JsonRpcMessage>();
        drop(tx);
        let (out, deliver) = collector();
        run_tail(rx, PARENT.into(), open_set(&["wf_1"]), cfg(), deliver).await;
        assert!(out.lock().unwrap().is_empty());
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

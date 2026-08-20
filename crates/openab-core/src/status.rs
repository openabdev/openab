use crate::adapter::{ChannelRef, ChatAdapter, MessageRef};
use std::sync::Arc;
use tokio::sync::Mutex;

const PROCESSING_TEXT: &str = "⏳ Processing…";
const MAX_TOOL_LABEL_CHARS: usize = 80;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusTerminal {
    Completed,
    Failed,
    TimedOut,
    DeliveryFailed,
}

impl StatusTerminal {
    fn text(self) -> &'static str {
        match self {
            Self::Completed => "✅ Completed",
            Self::Failed => "❌ Failed",
            Self::TimedOut => "⏱️ Timed out",
            Self::DeliveryFailed => "❌ Delivery failed",
        }
    }
}

enum State {
    Idle,
    Active {
        message: MessageRef,
        last_requested: String,
    },
    Terminal {
        message: MessageRef,
        last_requested: String,
    },
    Closed,
}

/// One turn-local processing message. The initial send returns the only
/// activity ID this controller may edit or delete; ambiguous writes never
/// trigger a fresh status send.
pub struct StatusMessageController {
    enabled: bool,
    adapter: Arc<dyn ChatAdapter>,
    channel: ChannelRef,
    state: Mutex<State>,
}

impl StatusMessageController {
    pub fn new(enabled: bool, adapter: Arc<dyn ChatAdapter>, channel: ChannelRef) -> Self {
        Self {
            enabled,
            adapter,
            channel,
            state: Mutex::new(State::Idle),
        }
    }

    pub async fn set_thinking(&self) {
        self.set_active(PROCESSING_TEXT).await;
    }

    pub async fn set_tool(&self, tool_name: &str) {
        let label = sanitize_tool_label(tool_name);
        self.set_active(&format!("🛠️ Using {label}…")).await;
    }

    pub async fn mark_terminal(&self, terminal: StatusTerminal) {
        if !self.enabled {
            return;
        }

        let next = terminal.text();
        let mut state = self.state.lock().await;
        let (message, last_requested) = match &*state {
            State::Idle => {
                *state = State::Closed;
                return;
            }
            State::Active {
                message,
                last_requested,
            }
            | State::Terminal {
                message,
                last_requested,
            } => (message.clone(), last_requested.clone()),
            State::Closed => return,
        };

        if last_requested != next {
            if let Err(error) = self.adapter.edit_message(&message, next).await {
                tracing::warn!(
                    error = ?error,
                    "processing status terminal update failed"
                );
            }
        }
        // Record the attempted state even when the PUT outcome is rejected or
        // unknown. A duplicate transition must not blindly retry the same write.
        *state = State::Terminal {
            message,
            last_requested: next.to_owned(),
        };
    }

    /// Delete only after the final content is fully delivered. The status was
    /// marked terminal first, so an explicit delete failure leaves recognizable
    /// terminal text rather than a live processing state whenever that PUT was
    /// delivered.
    pub async fn clear(&self) {
        if !self.enabled {
            return;
        }

        let mut state = self.state.lock().await;
        let previous = std::mem::replace(&mut *state, State::Closed);
        let message = match previous {
            State::Active { message, .. } | State::Terminal { message, .. } => message,
            State::Idle | State::Closed => return,
        };
        if let Err(error) = self.adapter.delete_message(&message).await {
            tracing::warn!(error = ?error, "processing status delete failed");
        }
    }

    async fn set_active(&self, text: &str) {
        if !self.enabled {
            return;
        }

        let mut state = self.state.lock().await;
        match &*state {
            State::Idle => match self.adapter.send_message(&self.channel, text).await {
                Ok(message) => {
                    *state = State::Active {
                        message,
                        last_requested: text.to_owned(),
                    };
                }
                Err(error) => {
                    // POST may have reached Teams. Disable this turn rather than
                    // fresh-send a duplicate status without a known activity ID.
                    tracing::warn!(error = ?error, "processing status create failed");
                    *state = State::Closed;
                }
            },
            State::Active {
                message,
                last_requested,
            } if last_requested != text => {
                let message = message.clone();
                if let Err(error) = self.adapter.edit_message(&message, text).await {
                    tracing::warn!(error = ?error, "processing status update failed");
                }
                // As with terminal PUTs, remember the attempted state so a
                // duplicate event cannot turn an ambiguous failure into a retry.
                *state = State::Active {
                    message,
                    last_requested: text.to_owned(),
                };
            }
            State::Active { .. } | State::Terminal { .. } | State::Closed => {}
        }
    }
}

fn sanitize_tool_label(value: &str) -> String {
    let normalized = value
        .replace('\r', "")
        .replace('\n', " ; ")
        .replace('`', "'");
    let collapsed = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    let label = collapsed
        .chars()
        .take(MAX_TOOL_LABEL_CHARS)
        .collect::<String>();
    if label.is_empty() {
        "tool".to_owned()
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{anyhow, Result};
    use async_trait::async_trait;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Mutex as StdMutex,
    };

    struct RecordingAdapter {
        events: StdMutex<Vec<String>>,
        fail_send: AtomicBool,
        fail_edit: AtomicBool,
        fail_delete: AtomicBool,
    }

    impl RecordingAdapter {
        fn new() -> Self {
            Self {
                events: StdMutex::new(Vec::new()),
                fail_send: AtomicBool::new(false),
                fail_edit: AtomicBool::new(false),
                fail_delete: AtomicBool::new(false),
            }
        }

        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ChatAdapter for RecordingAdapter {
        fn platform(&self) -> &'static str {
            "teams"
        }

        fn message_limit(&self) -> usize {
            4096
        }

        async fn send_message(&self, channel: &ChannelRef, content: &str) -> Result<MessageRef> {
            self.events.lock().unwrap().push(format!(
                "send:{content}:{}",
                channel.origin_event_id.as_deref().unwrap_or("none")
            ));
            if self.fail_send.load(Ordering::SeqCst) {
                return Err(anyhow!("send failed"));
            }
            Ok(MessageRef {
                channel: channel.clone(),
                message_id: "status-1".into(),
            })
        }

        async fn create_thread(
            &self,
            channel: &ChannelRef,
            _trigger_msg: &MessageRef,
            _title: &str,
        ) -> Result<ChannelRef> {
            Ok(channel.clone())
        }

        async fn add_reaction(&self, _msg: &MessageRef, _emoji: &str) -> Result<()> {
            Ok(())
        }

        async fn remove_reaction(&self, _msg: &MessageRef, _emoji: &str) -> Result<()> {
            Ok(())
        }

        async fn edit_message(&self, msg: &MessageRef, content: &str) -> Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("edit:{}:{content}", msg.message_id));
            if self.fail_edit.load(Ordering::SeqCst) {
                Err(anyhow!("edit failed"))
            } else {
                Ok(())
            }
        }

        async fn delete_message(&self, msg: &MessageRef) -> Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("delete:{}", msg.message_id));
            if self.fail_delete.load(Ordering::SeqCst) {
                Err(anyhow!("delete failed"))
            } else {
                Ok(())
            }
        }

        fn use_streaming(&self, _other_bot_present: bool) -> bool {
            false
        }
    }

    fn channel() -> ChannelRef {
        ChannelRef {
            platform: "teams".into(),
            channel_id: "conversation-1".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: Some("evt-last".into()),
        }
    }

    #[tokio::test]
    async fn lifecycle_reuses_one_real_message_and_marks_terminal_before_delete() {
        let adapter = Arc::new(RecordingAdapter::new());
        let controller = StatusMessageController::new(true, adapter.clone(), channel());

        controller.set_thinking().await;
        controller.set_tool("Read\n`src/main.rs`").await;
        controller.set_thinking().await;
        controller.mark_terminal(StatusTerminal::Completed).await;
        adapter
            .send_message(&channel(), "final answer")
            .await
            .unwrap();
        controller.clear().await;

        assert_eq!(
            adapter.events(),
            vec![
                "send:⏳ Processing…:evt-last",
                "edit:status-1:🛠️ Using Read ; 'src/main.rs'…",
                "edit:status-1:⏳ Processing…",
                "edit:status-1:✅ Completed",
                "send:final answer:evt-last",
                "delete:status-1",
            ]
        );
    }

    #[tokio::test]
    async fn ambiguous_initial_send_disables_status_without_fresh_send() {
        let adapter = Arc::new(RecordingAdapter::new());
        adapter.fail_send.store(true, Ordering::SeqCst);
        let controller = StatusMessageController::new(true, adapter.clone(), channel());

        controller.set_thinking().await;
        controller.set_tool("bash").await;
        controller.mark_terminal(StatusTerminal::Failed).await;
        controller.clear().await;

        assert_eq!(adapter.events(), vec!["send:⏳ Processing…:evt-last"]);
    }

    #[test]
    fn terminal_text_covers_every_outcome() {
        assert_eq!(StatusTerminal::Completed.text(), "✅ Completed");
        assert_eq!(StatusTerminal::Failed.text(), "❌ Failed");
        assert_eq!(StatusTerminal::TimedOut.text(), "⏱️ Timed out");
        assert_eq!(
            StatusTerminal::DeliveryFailed.text(),
            "❌ Delivery failed"
        );
    }

    #[tokio::test]
    async fn failed_put_is_not_blindly_retried() {
        let adapter = Arc::new(RecordingAdapter::new());
        let controller = StatusMessageController::new(true, adapter.clone(), channel());

        controller.set_thinking().await;
        adapter.fail_edit.store(true, Ordering::SeqCst);
        controller.set_tool("bash").await;
        controller.set_tool("bash").await;
        controller.mark_terminal(StatusTerminal::TimedOut).await;
        controller.mark_terminal(StatusTerminal::TimedOut).await;
        controller.clear().await;

        assert_eq!(
            adapter.events(),
            vec![
                "send:⏳ Processing…:evt-last",
                "edit:status-1:🛠️ Using bash…",
                "edit:status-1:⏱️ Timed out",
                "delete:status-1",
            ]
        );
    }

    #[tokio::test]
    async fn failed_delete_occurs_only_after_terminal_update() {
        let adapter = Arc::new(RecordingAdapter::new());
        adapter.fail_delete.store(true, Ordering::SeqCst);
        let controller = StatusMessageController::new(true, adapter.clone(), channel());

        controller.set_thinking().await;
        controller
            .mark_terminal(StatusTerminal::DeliveryFailed)
            .await;
        controller.clear().await;

        assert_eq!(
            adapter.events(),
            vec![
                "send:⏳ Processing…:evt-last",
                "edit:status-1:❌ Delivery failed",
                "delete:status-1",
            ]
        );
    }

    #[tokio::test]
    async fn disabled_controller_is_side_effect_free() {
        let adapter = Arc::new(RecordingAdapter::new());
        let controller = StatusMessageController::new(false, adapter.clone(), channel());

        controller.set_thinking().await;
        controller.mark_terminal(StatusTerminal::TimedOut).await;
        controller.clear().await;

        assert!(adapter.events().is_empty());
    }
}

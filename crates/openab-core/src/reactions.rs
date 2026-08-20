use crate::adapter::{ChatAdapter, MessageRef};
use crate::config::{ReactionEmojis, ReactionTiming};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::Duration;

const CODING_TOKENS: &[&str] = &["exec", "process", "read", "write", "edit", "bash", "shell"];
const WEB_TOKENS: &[&str] = &[
    "web_search",
    "web_fetch",
    "web-search",
    "web-fetch",
    "browser",
];

fn classify_tool<'a>(name: &str, emojis: &'a ReactionEmojis) -> &'a str {
    let n = name.to_lowercase();
    if WEB_TOKENS.iter().any(|t| n.contains(t)) {
        &emojis.web
    } else if CODING_TOKENS.iter().any(|t| n.contains(t)) {
        &emojis.coding
    } else {
        &emojis.tool
    }
}

struct Inner {
    adapter: Arc<dyn ChatAdapter>,
    message: MessageRef,
    emojis: ReactionEmojis,
    timing: ReactionTiming,
    current: String,
    finished: bool,
    debounce_handle: Option<tokio::task::JoinHandle<()>>,
    stall_soft_handle: Option<tokio::task::JoinHandle<()>>,
    stall_hard_handle: Option<tokio::task::JoinHandle<()>>,
}

pub struct StatusReactionController {
    inner: Arc<Mutex<Inner>>,
    enabled: bool,
}

impl StatusReactionController {
    pub fn new(
        enabled: bool,
        adapter: Arc<dyn ChatAdapter>,
        message: MessageRef,
        emojis: ReactionEmojis,
        timing: ReactionTiming,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                adapter,
                message,
                emojis,
                timing,
                current: String::new(),
                finished: false,
                debounce_handle: None,
                stall_soft_handle: None,
                stall_hard_handle: None,
            })),
            enabled,
        }
    }

    pub async fn set_queued(&self) {
        if !self.enabled {
            return;
        }
        let emoji = { self.inner.lock().await.emojis.queued.clone() };
        self.apply_immediate(&emoji).await;
    }

    pub async fn set_thinking(&self) {
        if !self.enabled {
            return;
        }
        let emoji = { self.inner.lock().await.emojis.thinking.clone() };
        self.schedule_debounced(&emoji).await;
    }

    pub async fn set_tool(&self, tool_name: &str) {
        if !self.enabled {
            return;
        }
        let emoji = {
            let inner = self.inner.lock().await;
            classify_tool(tool_name, &inner.emojis).to_string()
        };
        self.schedule_debounced(&emoji).await;
    }

    pub async fn set_done(&self) {
        if !self.enabled {
            return;
        }
        let emoji = { self.inner.lock().await.emojis.done.clone() };
        self.finish(&emoji).await;
        // Add a random mood face
        let faces = ["😊", "😎", "🫡", "🤓", "😏", "✌️", "💪", "🦾"];
        let face = faces[rand::random::<usize>() % faces.len()];
        let inner = self.inner.lock().await;
        let _ = inner.adapter.add_reaction(&inner.message, face).await;
    }

    pub async fn set_error(&self) {
        if !self.enabled {
            return;
        }
        let emoji = { self.inner.lock().await.emojis.error.clone() };
        self.finish(&emoji).await;
    }

    pub async fn clear(&self) {
        if !self.enabled {
            return;
        }
        let mut inner = self.inner.lock().await;
        cancel_timers(&mut inner);
        let current = inner.current.clone();
        if !current.is_empty() {
            let _ = inner
                .adapter
                .remove_reaction(&inner.message, &current)
                .await;
            inner.current.clear();
        }
    }

    async fn apply_immediate(&self, emoji: &str) {
        let mut inner = self.inner.lock().await;
        if inner.finished || emoji == inner.current {
            return;
        }
        cancel_debounce(&mut inner);
        let old = inner.current.clone();
        inner.current = emoji.to_string();
        let new = emoji.to_string();

        // Keep the controller lock for the complete swap. A later state must
        // not overtake add(new) -> remove(old), otherwise the old status can
        // become orphaned and remain visible forever.
        let _ = inner.adapter.add_reaction(&inner.message, &new).await;
        if !old.is_empty() && old != new {
            let _ = inner.adapter.remove_reaction(&inner.message, &old).await;
        }
        self.reset_stall_timers_inner(&mut inner);
    }

    async fn schedule_debounced(&self, emoji: &str) {
        let mut inner = self.inner.lock().await;
        if inner.finished || emoji == inner.current {
            self.reset_stall_timers_inner(&mut inner);
            return;
        }
        cancel_debounce(&mut inner);

        let emoji = emoji.to_string();
        let ctrl = self.inner.clone();
        let debounce_ms = inner.timing.debounce_ms;
        inner.debounce_handle = Some(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(debounce_ms)).await;
            let mut inner = ctrl.lock().await;
            if inner.finished {
                return;
            }
            // The handle only owns the pending delay. Once the delay fires,
            // detach this task so a later status update cannot abort it between
            // adding the new reaction and removing the previous one.
            let _ = inner.debounce_handle.take();
            let old = inner.current.clone();
            inner.current = emoji.clone();

            let _ = inner.adapter.add_reaction(&inner.message, &emoji).await;
            if !old.is_empty() && old != emoji {
                let _ = inner.adapter.remove_reaction(&inner.message, &old).await;
            }
        }));
        self.reset_stall_timers_inner(&mut inner);
    }

    async fn finish(&self, emoji: &str) {
        let mut inner = self.inner.lock().await;
        if inner.finished {
            return;
        }
        inner.finished = true;
        cancel_timers(&mut inner);

        let old = inner.current.clone();
        inner.current = emoji.to_string();
        let new = emoji.to_string();

        let _ = inner.adapter.add_reaction(&inner.message, &new).await;
        if !old.is_empty() && old != new {
            let _ = inner.adapter.remove_reaction(&inner.message, &old).await;
        }
    }

    fn reset_stall_timers_inner(&self, inner: &mut Inner) {
        if let Some(h) = inner.stall_soft_handle.take() {
            h.abort();
        }
        if let Some(h) = inner.stall_hard_handle.take() {
            h.abort();
        }

        let soft_ms = inner.timing.stall_soft_ms;
        let hard_ms = inner.timing.stall_hard_ms;
        let ctrl = self.inner.clone();

        inner.stall_soft_handle = Some(tokio::spawn({
            let ctrl = ctrl.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(soft_ms)).await;
                let mut inner = ctrl.lock().await;
                if inner.finished {
                    return;
                }
                let _ = inner.stall_soft_handle.take();
                let old = inner.current.clone();
                inner.current = "🥱".to_string();
                let _ = inner.adapter.add_reaction(&inner.message, "🥱").await;
                if !old.is_empty() && old != "🥱" {
                    let _ = inner.adapter.remove_reaction(&inner.message, &old).await;
                }
            }
        }));

        inner.stall_hard_handle = Some(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(hard_ms)).await;
            let mut inner = ctrl.lock().await;
            if inner.finished {
                return;
            }
            let _ = inner.stall_hard_handle.take();
            let old = inner.current.clone();
            inner.current = "😨".to_string();
            let _ = inner.adapter.add_reaction(&inner.message, "😨").await;
            if !old.is_empty() && old != "😨" {
                let _ = inner.adapter.remove_reaction(&inner.message, &old).await;
            }
        }));
    }
}

fn cancel_debounce(inner: &mut Inner) {
    if let Some(h) = inner.debounce_handle.take() {
        h.abort();
    }
}

fn cancel_timers(inner: &mut Inner) {
    if let Some(h) = inner.debounce_handle.take() {
        h.abort();
    }
    if let Some(h) = inner.stall_soft_handle.take() {
        h.abort();
    }
    if let Some(h) = inner.stall_hard_handle.take() {
        h.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::ChannelRef;
    use anyhow::{anyhow, Result};
    use async_trait::async_trait;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Mutex as StdMutex,
    };
    use tokio::sync::Notify;

    struct BlockingAdapter {
        events: StdMutex<Vec<String>>,
        thinking_add_started: Notify,
        release_thinking_add: Notify,
        blocked_once: AtomicBool,
    }

    impl BlockingAdapter {
        fn new() -> Self {
            Self {
                events: StdMutex::new(Vec::new()),
                thinking_add_started: Notify::new(),
                release_thinking_add: Notify::new(),
                blocked_once: AtomicBool::new(false),
            }
        }

        fn events(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
            self.events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }
    }

    #[async_trait]
    impl ChatAdapter for BlockingAdapter {
        fn platform(&self) -> &'static str {
            "test"
        }

        fn message_limit(&self) -> usize {
            2_000
        }

        async fn send_message(&self, _channel: &ChannelRef, _content: &str) -> Result<MessageRef> {
            Err(anyhow!("not used"))
        }

        async fn create_thread(
            &self,
            channel: &ChannelRef,
            _trigger_msg: &MessageRef,
            _title: &str,
        ) -> Result<ChannelRef> {
            Ok(channel.clone())
        }

        async fn add_reaction(&self, _msg: &MessageRef, emoji: &str) -> Result<()> {
            self.events().push(format!("add:{emoji}"));
            if emoji == "🤔" && !self.blocked_once.swap(true, Ordering::SeqCst) {
                self.thinking_add_started.notify_one();
                self.release_thinking_add.notified().await;
            }
            Ok(())
        }

        async fn remove_reaction(&self, _msg: &MessageRef, emoji: &str) -> Result<()> {
            self.events().push(format!("remove:{emoji}"));
            Ok(())
        }

        fn use_streaming(&self, _other_bot_present: bool) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn fired_debounce_finishes_reaction_swap_when_turn_finishes() {
        let adapter = Arc::new(BlockingAdapter::new());
        let message = MessageRef {
            channel: ChannelRef {
                platform: "test".into(),
                channel_id: "channel".into(),
                thread_id: None,
                parent_id: None,
                persistent_conversation: None,
                origin_event_id: None,
            },
            message_id: "message".into(),
        };
        let timing = ReactionTiming {
            debounce_ms: 0,
            stall_soft_ms: 60_000,
            stall_hard_ms: 60_000,
            ..ReactionTiming::default()
        };
        let controller = StatusReactionController::new(
            true,
            adapter.clone(),
            message,
            ReactionEmojis::default(),
            timing,
        );

        controller.set_queued().await;
        controller.set_thinking().await;
        let add_started = tokio::time::timeout(
            Duration::from_secs(1),
            adapter.thinking_add_started.notified(),
        )
        .await;
        assert!(add_started.is_ok(), "thinking reaction add did not start");

        // Finishing cancels pending timers. It must wait for a swap that has
        // already started instead of overtaking or aborting it midway.
        let controller = Arc::new(controller);
        let finish = tokio::spawn({
            let controller = controller.clone();
            async move { controller.set_error().await }
        });
        tokio::task::yield_now().await;
        adapter.release_thinking_add.notify_waiters();

        let finished = tokio::time::timeout(Duration::from_secs(1), finish).await;
        assert!(
            matches!(finished, Ok(Ok(()))),
            "final reaction transition did not finish"
        );
        let events = adapter.events();
        let queued_remove = events.iter().position(|event| event == "remove:👀");
        let error_add = events.iter().position(|event| event == "add:😱");
        assert!(
            matches!((queued_remove, error_add), (Some(remove), Some(add)) if remove < add),
            "final status overtook or omitted the in-flight reaction swap: {events:?}"
        );
    }
}

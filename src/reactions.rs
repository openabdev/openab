use crate::adapter::{ChatAdapter, MessageRef};
use crate::config::{ReactionEmojis, ReactionTiming};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::Duration;

const CODING_TOKENS: &[&str] = &["exec", "process", "read", "write", "edit", "bash", "shell"];
const WEB_TOKENS: &[&str] = &["web_search", "web_fetch", "web-search", "web-fetch", "browser"];

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
        if !self.enabled { return; }
        let emoji = { self.inner.lock().await.emojis.queued.clone() };
        self.apply_immediate(&emoji).await;
    }

    pub async fn set_thinking(&self) {
        if !self.enabled { return; }
        let emoji = { self.inner.lock().await.emojis.thinking.clone() };
        self.schedule_debounced(&emoji).await;
    }

    pub async fn set_tool(&self, tool_name: &str) {
        if !self.enabled { return; }
        let emoji = {
            let inner = self.inner.lock().await;
            classify_tool(tool_name, &inner.emojis).to_string()
        };
        self.schedule_debounced(&emoji).await;
    }

    pub async fn set_done(&self) {
        if !self.enabled { return; }
        let emoji = { self.inner.lock().await.emojis.done.clone() };
        self.finish(&emoji).await;
        // Add a random mood face
        let faces = ["😊", "😎", "🫡", "🤓", "😏", "✌️", "💪", "🦾"];
        let face = faces[rand::random::<usize>() % faces.len()];
        let inner = self.inner.lock().await;
        let _ = inner.adapter.add_reaction(&inner.message, face).await;
    }

    pub async fn set_error(&self) {
        if !self.enabled { return; }
        let emoji = { self.inner.lock().await.emojis.error.clone() };
        self.finish(&emoji).await;
    }

    pub async fn clear(&self) {
        if !self.enabled { return; }
        let mut inner = self.inner.lock().await;
        cancel_timers(&mut inner);
        let current = inner.current.clone();
        if !current.is_empty() {
            let _ = inner.adapter.remove_reaction(&inner.message, &current).await;
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
        let adapter = inner.adapter.clone();
        let msg = inner.message.clone();
        let new = emoji.to_string();
        drop(inner);

        let _ = adapter.add_reaction(&msg, &new).await;
        if !old.is_empty() && old != new {
            let _ = adapter.remove_reaction(&msg, &old).await;
        }
        self.reset_stall_timers().await;
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
            if inner.finished { return; }
            let old = inner.current.clone();
            inner.current = emoji.clone();
            let adapter = inner.adapter.clone();
            let msg = inner.message.clone();
            drop(inner);

            let _ = adapter.add_reaction(&msg, &emoji).await;
            if !old.is_empty() && old != emoji {
                let _ = adapter.remove_reaction(&msg, &old).await;
            }
        }));
        self.reset_stall_timers_inner(&mut inner);
    }

    async fn finish(&self, emoji: &str) {
        let mut inner = self.inner.lock().await;
        if inner.finished { return; }
        inner.finished = true;
        cancel_timers(&mut inner);

        let old = inner.current.clone();
        inner.current = emoji.to_string();
        let adapter = inner.adapter.clone();
        let msg = inner.message.clone();
        let new = emoji.to_string();
        drop(inner);

        let _ = adapter.add_reaction(&msg, &new).await;
        if !old.is_empty() && old != new {
            let _ = adapter.remove_reaction(&msg, &old).await;
        }
    }

    async fn reset_stall_timers(&self) {
        let mut inner = self.inner.lock().await;
        self.reset_stall_timers_inner(&mut inner);
    }

    fn reset_stall_timers_inner(&self, inner: &mut Inner) {
        if let Some(h) = inner.stall_soft_handle.take() { h.abort(); }
        if let Some(h) = inner.stall_hard_handle.take() { h.abort(); }

        let soft_ms = inner.timing.stall_soft_ms;
        let hard_ms = inner.timing.stall_hard_ms;
        let ctrl = self.inner.clone();

        inner.stall_soft_handle = Some(tokio::spawn({
            let ctrl = ctrl.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(soft_ms)).await;
                let mut inner = ctrl.lock().await;
                if inner.finished { return; }
                let old = inner.current.clone();
                inner.current = "🥱".to_string();
                let adapter = inner.adapter.clone();
                let msg = inner.message.clone();
                drop(inner);
                let _ = adapter.add_reaction(&msg, "🥱").await;
                if !old.is_empty() && old != "🥱" {
                    let _ = adapter.remove_reaction(&msg, &old).await;
                }
            }
        }));

        inner.stall_hard_handle = Some(tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(hard_ms)).await;
            let mut inner = ctrl.lock().await;
            if inner.finished { return; }
            let old = inner.current.clone();
            inner.current = "😨".to_string();
            let adapter = inner.adapter.clone();
            let msg = inner.message.clone();
            drop(inner);
            let _ = adapter.add_reaction(&msg, "😨").await;
            if !old.is_empty() && old != "😨" {
                let _ = adapter.remove_reaction(&msg, &old).await;
            }
        }));
    }
}

fn cancel_debounce(inner: &mut Inner) {
    if let Some(h) = inner.debounce_handle.take() { h.abort(); }
}

fn cancel_timers(inner: &mut Inner) {
    if let Some(h) = inner.debounce_handle.take() { h.abort(); }
    if let Some(h) = inner.stall_soft_handle.take() { h.abort(); }
    if let Some(h) = inner.stall_hard_handle.take() { h.abort(); }
}

use super::{
    ContextConfig, ContextFetchRequest, ContextMessage, ContextObserveRequest, ContextProvider,
    ContextScope,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Debug)]
struct BufferedContextMessage {
    message: ContextMessage,
    observed_at: Instant,
}

#[derive(Clone, Debug)]
pub struct BufferedContextProvider {
    config: ContextConfig,
    buffers: Arc<std::sync::Mutex<HashMap<ContextScope, VecDeque<BufferedContextMessage>>>>,
}

impl BufferedContextProvider {
    pub fn new(config: ContextConfig) -> Self {
        Self {
            config,
            buffers: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    #[cfg(test)]
    pub fn buffered_texts(&self, scope: &ContextScope) -> Vec<String> {
        let guard = self.buffers.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .get(scope)
            .map(|entry| {
                entry
                    .iter()
                    .map(|message| message.message.text.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub fn buffered_len(&self, scope: &ContextScope) -> usize {
        let guard = self.buffers.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(scope).map(VecDeque::len).unwrap_or_default()
    }

    fn prune_expired(&self, entry: &mut VecDeque<BufferedContextMessage>) {
        let now = Instant::now();
        entry.retain(|m| now.duration_since(m.observed_at).as_secs() < self.config.ttl_secs);
    }

    fn enforce_limits(&self, entry: &mut VecDeque<BufferedContextMessage>, max_messages: usize) {
        while entry.len() > max_messages {
            entry.pop_front();
        }
        while entry.len() > 1 && context_char_count(entry) > self.config.max_chars {
            entry.pop_front();
        }
    }
}

#[async_trait::async_trait]
impl ContextProvider for BufferedContextProvider {
    fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    async fn observe(&self, request: ContextObserveRequest) -> bool {
        let trimmed = request.text.trim();
        if !self.config.enabled
            || self.config.max_messages == 0
            || self.config.max_chars == 0
            || trimmed.is_empty()
        {
            return false;
        }

        let mut guard = self.buffers.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.entry(request.scope).or_default();
        self.prune_expired(entry);

        let bounded_text: String = trimmed.chars().take(self.config.max_chars).collect();
        entry.push_back(BufferedContextMessage {
            message: ContextMessage {
                sender_id: request.sender_id,
                sender_label: request.sender_label,
                text: bounded_text,
            },
            observed_at: Instant::now(),
        });
        self.enforce_limits(entry, self.config.max_messages);
        true
    }

    async fn fetch_context(&self, request: ContextFetchRequest) -> Option<Vec<ContextMessage>> {
        if !self.config.enabled || self.config.max_messages == 0 || self.config.max_chars == 0 {
            return None;
        }

        let mut guard = self.buffers.lock().unwrap_or_else(|e| e.into_inner());
        let mut entry = guard.remove(&request.scope)?;
        self.prune_expired(&mut entry);

        let max_messages = request
            .limit
            .unwrap_or(self.config.max_messages)
            .min(self.config.max_messages);
        self.enforce_limits(&mut entry, max_messages);

        if entry.is_empty() {
            None
        } else {
            Some(entry.into_iter().map(|message| message.message).collect())
        }
    }
}

fn context_char_count(entry: &VecDeque<BufferedContextMessage>) -> usize {
    entry
        .iter()
        .map(|m| {
            m.message.sender_label.chars().count()
                + m.message.sender_id.chars().count()
                + m.message.text.chars().count()
                + 2
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_provider(max_messages: usize, max_chars: usize) -> BufferedContextProvider {
        BufferedContextProvider::new(ContextConfig {
            enabled: true,
            ttl_secs: 24 * 60 * 60,
            max_messages,
            max_chars,
        })
    }

    fn scope(channel: &str) -> ContextScope {
        ContextScope::new("line", channel, None, "line-default-bot")
    }

    async fn observe(provider: &BufferedContextProvider, scope: ContextScope, text: &str) {
        provider
            .observe(ContextObserveRequest {
                scope,
                sender_id: "U_sender".into(),
                sender_label: "U_sender".into(),
                text: text.into(),
            })
            .await;
    }

    #[tokio::test]
    async fn observe_fetches_and_drains_context() {
        let provider = enabled_provider(50, 8_000);
        let scope = scope("C001");

        observe(&provider, scope.clone(), "hello").await;
        let context = provider
            .fetch_context(ContextFetchRequest {
                scope: scope.clone(),
                limit: None,
            })
            .await
            .expect("context should be returned");

        assert_eq!(context.len(), 1);
        assert_eq!(context[0].text, "hello");
        assert!(provider
            .fetch_context(ContextFetchRequest { scope, limit: None })
            .await
            .is_none());
    }

    #[tokio::test]
    async fn observe_is_noop_when_disabled() {
        let provider = BufferedContextProvider::new(ContextConfig::default());
        let scope = scope("C001");

        observe(&provider, scope.clone(), "hello").await;

        assert_eq!(provider.buffered_len(&scope), 0);
    }

    #[tokio::test]
    async fn max_messages_keeps_latest_context() {
        let provider = enabled_provider(2, 8_000);
        let scope = scope("C001");

        observe(&provider, scope.clone(), "first").await;
        observe(&provider, scope.clone(), "second").await;
        observe(&provider, scope.clone(), "third").await;

        assert_eq!(provider.buffered_texts(&scope), vec!["second", "third"]);
    }

    #[tokio::test]
    async fn max_chars_trims_old_context() {
        let provider = enabled_provider(10, 20);
        let scope = scope("C001");

        observe(&provider, scope.clone(), "first long message").await;
        observe(&provider, scope.clone(), "second long message").await;

        let texts = provider.buffered_texts(&scope);
        assert!(texts.len() <= 2);
        assert_eq!(
            texts.last().map(String::as_str),
            Some("second long message")
        );
    }

    #[tokio::test]
    async fn scope_isolation_prevents_cross_chat_leakage() {
        let provider = enabled_provider(50, 8_000);
        let first = scope("C001");
        let second = scope("C002");

        observe(&provider, first.clone(), "first chat").await;

        assert!(provider
            .fetch_context(ContextFetchRequest {
                scope: second,
                limit: None,
            })
            .await
            .is_none());
        assert!(provider
            .fetch_context(ContextFetchRequest {
                scope: first,
                limit: None,
            })
            .await
            .is_some());
    }
}

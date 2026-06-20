pub mod api_fetch;
pub mod buffered;
pub mod config;

pub use buffered::BufferedContextProvider;
pub use config::ContextConfig;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ContextScope {
    pub platform: String,
    pub channel_id: String,
    pub thread_id: Option<String>,
    pub bot_id: String,
}

impl ContextScope {
    pub fn new(
        platform: impl Into<String>,
        channel_id: impl Into<String>,
        thread_id: Option<String>,
        bot_id: impl Into<String>,
    ) -> Self {
        Self {
            platform: platform.into(),
            channel_id: channel_id.into(),
            thread_id,
            bot_id: bot_id.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ContextMessage {
    pub sender_id: String,
    pub sender_label: String,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct ContextObserveRequest {
    pub scope: ContextScope,
    pub sender_id: String,
    pub sender_label: String,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct ContextFetchRequest {
    pub scope: ContextScope,
    pub limit: Option<usize>,
}

#[async_trait::async_trait]
pub trait ContextProvider: Send + Sync {
    fn is_enabled(&self) -> bool;

    async fn observe(&self, request: ContextObserveRequest) -> bool;

    async fn fetch_context(&self, request: ContextFetchRequest) -> Option<Vec<ContextMessage>>;
}

pub fn inject_context(context: &[ContextMessage], current_text: &str) -> String {
    if context.is_empty() {
        return current_text.to_string();
    }

    let mut lines = Vec::with_capacity(context.len() + 3);
    lines.push("[Recent conversation context before this trigger]".to_string());
    for message in context {
        let label = if message.sender_label.trim().is_empty() {
            message.sender_id.as_str()
        } else {
            message.sender_label.as_str()
        };
        lines.push(format!("{}: {}", label, message.text));
    }
    lines.push(String::new());
    lines.push("[Current message - respond to this]".to_string());
    lines.push(current_text.to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_context_wraps_history_and_current_message() {
        let text = inject_context(
            &[
                ContextMessage {
                    sender_id: "U1".into(),
                    sender_label: "Alice".into(),
                    text: "first".into(),
                },
                ContextMessage {
                    sender_id: "U2".into(),
                    sender_label: "Bob".into(),
                    text: "second".into(),
                },
            ],
            "@Bot summarize",
        );

        assert!(text.contains("[Recent conversation context before this trigger]"));
        assert!(text.contains("Alice: first"));
        assert!(text.contains("Bob: second"));
        assert!(text.contains("[Current message - respond to this]"));
        assert!(text.contains("@Bot summarize"));
    }

    #[test]
    fn inject_context_keeps_current_message_when_history_empty() {
        assert_eq!(inject_context(&[], "hello"), "hello");
    }
}

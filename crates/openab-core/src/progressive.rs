use crate::adapter::{ChannelRef, ChatAdapter, MessageRef, WriteOutcome};
use std::sync::Arc;

pub(crate) const COSMETIC_EDIT_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(1500);
const MAX_CONSECUTIVE_EDIT_FAILURES: u32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CosmeticEditOutcome {
    Delivered,
    Rejected,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FinalEditPlan {
    Put,
    AlreadyDelivered,
    RecoverRejected,
    Ambiguous,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CosmeticEditState {
    last_attempted: String,
    last_outcome: Option<CosmeticEditOutcome>,
    consecutive_failures: u32,
}

impl CosmeticEditState {
    /// Reserve one changed display value before awaiting its PUT. The
    /// provisional outcome is Unknown so task cancellation cannot turn an
    /// in-flight write into a duplicate final PUT.
    pub fn begin_attempt(&mut self, content: String) -> bool {
        if content == self.last_attempted {
            return false;
        }
        self.last_attempted = content;
        self.last_outcome = Some(CosmeticEditOutcome::Unknown);
        true
    }

    /// Complete the reserved PUT. Returns true when cosmetic streaming must
    /// stop for this turn.
    pub fn complete_attempt(&mut self, outcome: CosmeticEditOutcome) -> bool {
        self.last_outcome = Some(outcome);
        if outcome == CosmeticEditOutcome::Delivered {
            self.consecutive_failures = 0;
        } else {
            self.consecutive_failures += 1;
        }
        self.consecutive_failures >= MAX_CONSECUTIVE_EDIT_FAILURES
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    fn final_edit_plan(&self, final_content: &str) -> FinalEditPlan {
        if self.last_attempted != final_content {
            return FinalEditPlan::Put;
        }
        match self.last_outcome {
            Some(CosmeticEditOutcome::Delivered) => FinalEditPlan::AlreadyDelivered,
            Some(CosmeticEditOutcome::Rejected) => FinalEditPlan::RecoverRejected,
            Some(CosmeticEditOutcome::Unknown) => FinalEditPlan::Ambiguous,
            None => FinalEditPlan::Put,
        }
    }
}

#[derive(Debug)]
pub(crate) struct AmbiguousProgressiveDelivery;

impl std::fmt::Display for AmbiguousProgressiveDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "progressive delivery outcome is ambiguous")
    }
}

impl std::error::Error for AmbiguousProgressiveDelivery {}

pub(crate) fn is_ambiguous_delivery(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<AmbiguousProgressiveDelivery>()
        .is_some()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ProgressiveDelivery {
    pub failed: bool,
    pub ambiguous: bool,
}

impl ProgressiveDelivery {
    fn rejected() -> Self {
        Self {
            failed: true,
            ambiguous: false,
        }
    }

    fn unknown() -> Self {
        Self {
            failed: true,
            ambiguous: true,
        }
    }
}

#[derive(Debug)]
pub(crate) enum PlaceholderStart {
    Ready(MessageRef),
    Rejected,
    Unknown,
}

pub(crate) fn classify_placeholder(
    channel: &ChannelRef,
    outcome: WriteOutcome,
) -> PlaceholderStart {
    match outcome {
        WriteOutcome::Delivered {
            message_id: Some(message_id),
        } if !message_id.is_empty() => PlaceholderStart::Ready(MessageRef {
            channel: channel.clone(),
            message_id,
        }),
        WriteOutcome::Delivered { .. } | WriteOutcome::Unknown { .. } => PlaceholderStart::Unknown,
        WriteOutcome::Rejected { .. } => PlaceholderStart::Rejected,
    }
}

fn delivered(outcome: &WriteOutcome) -> bool {
    matches!(
        outcome,
        WriteOutcome::Delivered {
            message_id: Some(message_id)
        } if !message_id.is_empty()
    )
}

pub(crate) async fn deliver_fresh_chunks(
    adapter: &Arc<dyn ChatAdapter>,
    channel: &ChannelRef,
    chunks: &[String],
) -> ProgressiveDelivery {
    for chunk in chunks {
        match adapter.send_message_outcome(channel, chunk).await {
            outcome if delivered(&outcome) => {}
            WriteOutcome::Rejected { code, .. } => {
                tracing::warn!(error_code = %code, "progressive fresh chunk rejected");
                return ProgressiveDelivery::rejected();
            }
            WriteOutcome::Delivered { .. } | WriteOutcome::Unknown { .. } => {
                tracing::warn!("progressive fresh chunk outcome unknown; stopping delivery");
                return ProgressiveDelivery::unknown();
            }
        }
    }
    ProgressiveDelivery::default()
}

async fn recover_rejected_placeholder(
    adapter: &Arc<dyn ChatAdapter>,
    channel: &ChannelRef,
    placeholder: &MessageRef,
    chunks: &[String],
) -> ProgressiveDelivery {
    match adapter.delete_message_outcome(placeholder).await {
        WriteOutcome::Unknown { code, .. } => {
            tracing::warn!(
                error_code = %code,
                "placeholder delete outcome unknown; not fresh-sending"
            );
            ProgressiveDelivery::unknown()
        }
        WriteOutcome::Delivered { .. } => deliver_fresh_chunks(adapter, channel, chunks).await,
        WriteOutcome::Rejected { code, .. } => {
            tracing::warn!(
                error_code = %code,
                "placeholder delete rejected; fresh answer may overlap partial content"
            );
            deliver_fresh_chunks(adapter, channel, chunks).await
        }
    }
}

pub(crate) async fn finalize_edit_placeholder(
    adapter: &Arc<dyn ChatAdapter>,
    channel: &ChannelRef,
    placeholder: &MessageRef,
    chunks: &[String],
) -> ProgressiveDelivery {
    let Some(first) = chunks.first() else {
        return ProgressiveDelivery::default();
    };

    match adapter.edit_message_outcome(placeholder, first).await {
        WriteOutcome::Delivered { .. } => {
            deliver_fresh_chunks(adapter, channel, &chunks[1..]).await
        }
        WriteOutcome::Unknown { code, .. } => {
            tracing::warn!(
                error_code = %code,
                "final progressive edit outcome unknown; not deleting or fresh-sending"
            );
            ProgressiveDelivery::unknown()
        }
        WriteOutcome::Rejected { code, .. } => {
            tracing::warn!(
                error_code = %code,
                "final progressive edit rejected; attempting placeholder recovery"
            );
            recover_rejected_placeholder(adapter, channel, placeholder, chunks).await
        }
    }
}

pub(crate) async fn finalize_edit_after_cosmetic(
    adapter: &Arc<dyn ChatAdapter>,
    channel: &ChannelRef,
    placeholder: &MessageRef,
    chunks: &[String],
    cosmetic: Option<&CosmeticEditState>,
) -> ProgressiveDelivery {
    let Some(first) = chunks.first() else {
        return ProgressiveDelivery::default();
    };
    let plan = cosmetic
        .map(|state| state.final_edit_plan(first))
        .unwrap_or(FinalEditPlan::Put);

    match plan {
        FinalEditPlan::Put => {
            finalize_edit_placeholder(adapter, channel, placeholder, chunks).await
        }
        FinalEditPlan::AlreadyDelivered => {
            deliver_fresh_chunks(adapter, channel, &chunks[1..]).await
        }
        FinalEditPlan::RecoverRejected => {
            tracing::warn!(
                "last cosmetic edit explicitly rejected the final content; recovering without retry"
            );
            recover_rejected_placeholder(adapter, channel, placeholder, chunks).await
        }
        FinalEditPlan::Ambiguous => {
            tracing::warn!(
                "last cosmetic edit may already contain the final content; not retrying or recovering"
            );
            ProgressiveDelivery::unknown()
        }
    }
}

pub(crate) async fn deliver_explicit_reply_chunks(
    adapter: &Arc<dyn ChatAdapter>,
    channel: &ChannelRef,
    reply_to_message_id: &str,
    chunks: &[String],
) -> ProgressiveDelivery {
    let Some(first) = chunks.first() else {
        return ProgressiveDelivery::default();
    };

    match adapter
        .send_message_with_reply_outcome(channel, first, reply_to_message_id)
        .await
    {
        outcome if delivered(&outcome) => {}
        WriteOutcome::Rejected { code, .. } => {
            tracing::warn!(error_code = %code, "progressive explicit reply rejected");
            return ProgressiveDelivery::rejected();
        }
        WriteOutcome::Delivered { .. } | WriteOutcome::Unknown { .. } => {
            tracing::warn!("progressive explicit reply outcome unknown");
            return ProgressiveDelivery::unknown();
        }
    }

    deliver_fresh_chunks(adapter, channel, &chunks[1..]).await
}

pub(crate) async fn finalize_explicit_reply(
    adapter: &Arc<dyn ChatAdapter>,
    channel: &ChannelRef,
    placeholder: &MessageRef,
    reply_to_message_id: &str,
    chunks: &[String],
) -> ProgressiveDelivery {
    let delivery =
        deliver_explicit_reply_chunks(adapter, channel, reply_to_message_id, chunks).await;
    if delivery.failed {
        return delivery;
    }

    // Every final-content chunk is already authoritative at this point. Cleanup
    // may leave an orphan, but it must not retry or downgrade delivered content.
    match adapter.delete_message_outcome(placeholder).await {
        WriteOutcome::Delivered { .. } => {}
        WriteOutcome::Rejected { code, .. } => {
            tracing::warn!(
                error_code = %code,
                "explicit reply delivered but placeholder delete was rejected"
            );
        }
        WriteOutcome::Unknown { code, .. } => {
            tracing::warn!(
                error_code = %code,
                "explicit reply delivered but placeholder delete outcome is unknown"
            );
        }
    }
    ProgressiveDelivery::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{anyhow, Result};
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct RecordingAdapter {
        events: Mutex<Vec<String>>,
        sends: Mutex<VecDeque<WriteOutcome>>,
        edits: Mutex<VecDeque<WriteOutcome>>,
        deletes: Mutex<VecDeque<WriteOutcome>>,
        replies: Mutex<VecDeque<WriteOutcome>>,
    }

    impl RecordingAdapter {
        fn new() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
                sends: Mutex::new(VecDeque::new()),
                edits: Mutex::new(VecDeque::new()),
                deletes: Mutex::new(VecDeque::new()),
                replies: Mutex::new(VecDeque::new()),
            }
        }

        fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
            mutex.lock().expect("recording adapter mutex poisoned")
        }

        fn pop_outcome(queue: &Mutex<VecDeque<WriteOutcome>>) -> WriteOutcome {
            Self::lock(queue)
                .pop_front()
                .expect("missing queued write outcome")
        }

        fn events(&self) -> Vec<String> {
            Self::lock(&self.events).clone()
        }

        fn push_send(&self, outcome: WriteOutcome) {
            Self::lock(&self.sends).push_back(outcome);
        }

        fn push_edit(&self, outcome: WriteOutcome) {
            Self::lock(&self.edits).push_back(outcome);
        }

        fn push_delete(&self, outcome: WriteOutcome) {
            Self::lock(&self.deletes).push_back(outcome);
        }

        fn push_reply(&self, outcome: WriteOutcome) {
            Self::lock(&self.replies).push_back(outcome);
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

        async fn send_message(&self, _channel: &ChannelRef, _content: &str) -> Result<MessageRef> {
            Err(anyhow!("use outcome method"))
        }

        async fn send_message_outcome(&self, _channel: &ChannelRef, content: &str) -> WriteOutcome {
            Self::lock(&self.events).push(format!("send:{content}"));
            Self::pop_outcome(&self.sends)
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

        async fn edit_message_outcome(&self, _msg: &MessageRef, content: &str) -> WriteOutcome {
            Self::lock(&self.events).push(format!("edit:{content}"));
            Self::pop_outcome(&self.edits)
        }

        async fn delete_message_outcome(&self, _msg: &MessageRef) -> WriteOutcome {
            Self::lock(&self.events).push("delete".into());
            Self::pop_outcome(&self.deletes)
        }

        async fn send_message_with_reply_outcome(
            &self,
            _channel: &ChannelRef,
            content: &str,
            _reply_to_message_id: &str,
        ) -> WriteOutcome {
            Self::lock(&self.events).push(format!("reply:{content}"));
            Self::pop_outcome(&self.replies)
        }

        fn use_streaming(&self, _other_bot_present: bool) -> bool {
            true
        }
    }

    fn channel() -> ChannelRef {
        ChannelRef {
            platform: "teams".into(),
            channel_id: "conversation".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: Some("event".into()),
        }
    }

    fn placeholder() -> MessageRef {
        MessageRef {
            channel: channel(),
            message_id: "placeholder".into(),
        }
    }

    fn delivered(id: &str) -> WriteOutcome {
        WriteOutcome::Delivered {
            message_id: Some(id.into()),
        }
    }

    fn rejected() -> WriteOutcome {
        WriteOutcome::Rejected {
            code: "rejected".into(),
            message: "no".into(),
            retry_after_ms: None,
        }
    }

    fn unknown() -> WriteOutcome {
        WriteOutcome::Unknown {
            code: "unknown".into(),
            message: "maybe".into(),
        }
    }

    #[test]
    fn ambiguity_marker_survives_anyhow_erasure() {
        let error = anyhow::Error::new(AmbiguousProgressiveDelivery);
        assert!(is_ambiguous_delivery(&error));
        assert!(!is_ambiguous_delivery(&anyhow!("ordinary failure")));
    }

    #[test]
    fn cosmetic_edit_state_never_retries_the_same_failed_content() {
        let mut state = CosmeticEditState::default();
        assert!(state.begin_attempt("first".into()));
        assert!(!state.complete_attempt(CosmeticEditOutcome::Unknown));
        assert!(!state.begin_attempt("first".into()));
        assert!(state.begin_attempt("second".into()));
        assert_eq!(state.consecutive_failures(), 1);
        assert_eq!(COSMETIC_EDIT_INTERVAL.as_millis(), 1500);
    }

    #[test]
    fn cosmetic_edit_state_stops_after_three_distinct_failures_and_success_resets() {
        let mut state = CosmeticEditState::default();
        for (content, stop) in [("one", false), ("two", false), ("three", true)] {
            assert!(state.begin_attempt(content.into()));
            assert_eq!(state.complete_attempt(CosmeticEditOutcome::Rejected), stop);
        }

        let mut reset = CosmeticEditState::default();
        assert!(reset.begin_attempt("one".into()));
        assert!(!reset.complete_attempt(CosmeticEditOutcome::Unknown));
        assert!(reset.begin_attempt("two".into()));
        assert!(!reset.complete_attempt(CosmeticEditOutcome::Delivered));
        assert_eq!(reset.consecutive_failures(), 0);
        assert!(reset.begin_attempt("three".into()));
        assert!(!reset.complete_attempt(CosmeticEditOutcome::Rejected));
    }

    #[tokio::test]
    async fn delivered_same_content_is_not_put_twice() {
        let adapter = Arc::new(RecordingAdapter::new());
        let erased: Arc<dyn ChatAdapter> = adapter.clone();
        let mut state = CosmeticEditState::default();
        assert!(state.begin_attempt("final".into()));
        state.complete_attempt(CosmeticEditOutcome::Delivered);

        let result = finalize_edit_after_cosmetic(
            &erased,
            &channel(),
            &placeholder(),
            &["final".into()],
            Some(&state),
        )
        .await;

        assert_eq!(result, ProgressiveDelivery::default());
        assert!(adapter.events().is_empty());
    }

    #[tokio::test]
    async fn rejected_same_content_recovers_without_repeating_put() {
        let adapter = Arc::new(RecordingAdapter::new());
        adapter.push_delete(WriteOutcome::Delivered { message_id: None });
        adapter.push_send(delivered("fresh"));
        let erased: Arc<dyn ChatAdapter> = adapter.clone();
        let mut state = CosmeticEditState::default();
        assert!(state.begin_attempt("final".into()));
        state.complete_attempt(CosmeticEditOutcome::Rejected);

        let result = finalize_edit_after_cosmetic(
            &erased,
            &channel(),
            &placeholder(),
            &["final".into()],
            Some(&state),
        )
        .await;

        assert_eq!(result, ProgressiveDelivery::default());
        assert_eq!(adapter.events(), vec!["delete", "send:final"]);
    }

    #[tokio::test]
    async fn in_flight_same_content_is_ambiguous_without_another_write() {
        let adapter = Arc::new(RecordingAdapter::new());
        let erased: Arc<dyn ChatAdapter> = adapter.clone();
        let mut state = CosmeticEditState::default();
        assert!(state.begin_attempt("final".into()));

        let result = finalize_edit_after_cosmetic(
            &erased,
            &channel(),
            &placeholder(),
            &["final".into()],
            Some(&state),
        )
        .await;

        assert_eq!(result, ProgressiveDelivery::unknown());
        assert!(adapter.events().is_empty());
    }

    #[tokio::test]
    async fn newer_final_content_may_supersede_an_unknown_cosmetic_put() {
        let adapter = Arc::new(RecordingAdapter::new());
        adapter.push_edit(WriteOutcome::Delivered { message_id: None });
        let erased: Arc<dyn ChatAdapter> = adapter.clone();
        let mut state = CosmeticEditState::default();
        assert!(state.begin_attempt("partial".into()));
        state.complete_attempt(CosmeticEditOutcome::Unknown);

        let result = finalize_edit_after_cosmetic(
            &erased,
            &channel(),
            &placeholder(),
            &["final".into()],
            Some(&state),
        )
        .await;

        assert_eq!(result, ProgressiveDelivery::default());
        assert_eq!(adapter.events(), vec!["edit:final"]);
    }

    #[test]
    fn placeholder_requires_a_non_empty_real_id() {
        assert!(matches!(
            classify_placeholder(&channel(), delivered("real")),
            PlaceholderStart::Ready(message) if message.message_id == "real"
        ));
        assert!(matches!(
            classify_placeholder(&channel(), delivered("")),
            PlaceholderStart::Unknown
        ));
        assert!(matches!(
            classify_placeholder(&channel(), rejected()),
            PlaceholderStart::Rejected
        ));
        assert!(matches!(
            classify_placeholder(&channel(), unknown()),
            PlaceholderStart::Unknown
        ));
    }

    #[tokio::test]
    async fn delivered_final_edit_sends_overflow_in_order() {
        let adapter = Arc::new(RecordingAdapter::new());
        adapter.push_edit(WriteOutcome::Delivered { message_id: None });
        adapter.push_send(delivered("overflow-1"));
        adapter.push_send(delivered("overflow-2"));
        let erased: Arc<dyn ChatAdapter> = adapter.clone();

        let result = finalize_edit_placeholder(
            &erased,
            &channel(),
            &placeholder(),
            &["first".into(), "second".into(), "third".into()],
        )
        .await;

        assert_eq!(result, ProgressiveDelivery::default());
        assert_eq!(
            adapter.events(),
            vec!["edit:first", "send:second", "send:third"]
        );
    }

    #[tokio::test]
    async fn rejected_edit_and_delivered_delete_fresh_send_once() {
        let adapter = Arc::new(RecordingAdapter::new());
        adapter.push_edit(rejected());
        adapter.push_delete(WriteOutcome::Delivered { message_id: None });
        adapter.push_send(delivered("fresh"));
        let erased: Arc<dyn ChatAdapter> = adapter.clone();

        let result =
            finalize_edit_placeholder(&erased, &channel(), &placeholder(), &["final".into()]).await;

        assert_eq!(result, ProgressiveDelivery::default());
        assert_eq!(adapter.events(), vec!["edit:final", "delete", "send:final"]);
    }

    #[tokio::test]
    async fn rejected_delete_still_delivers_one_complete_fresh_answer() {
        let adapter = Arc::new(RecordingAdapter::new());
        adapter.push_edit(rejected());
        adapter.push_delete(rejected());
        adapter.push_send(delivered("fresh"));
        let erased: Arc<dyn ChatAdapter> = adapter.clone();

        let result =
            finalize_edit_placeholder(&erased, &channel(), &placeholder(), &["final".into()]).await;

        assert_eq!(result, ProgressiveDelivery::default());
        assert_eq!(adapter.events(), vec!["edit:final", "delete", "send:final"]);
    }

    #[tokio::test]
    async fn unknown_edit_never_deletes_or_fresh_sends() {
        let adapter = Arc::new(RecordingAdapter::new());
        adapter.push_edit(unknown());
        let erased: Arc<dyn ChatAdapter> = adapter.clone();

        let result =
            finalize_edit_placeholder(&erased, &channel(), &placeholder(), &["final".into()]).await;

        assert_eq!(result, ProgressiveDelivery::unknown());
        assert_eq!(adapter.events(), vec!["edit:final"]);
    }

    #[tokio::test]
    async fn unknown_delete_after_rejected_edit_never_fresh_sends() {
        let adapter = Arc::new(RecordingAdapter::new());
        adapter.push_edit(rejected());
        adapter.push_delete(unknown());
        let erased: Arc<dyn ChatAdapter> = adapter.clone();

        let result =
            finalize_edit_placeholder(&erased, &channel(), &placeholder(), &["final".into()]).await;

        assert_eq!(result, ProgressiveDelivery::unknown());
        assert_eq!(adapter.events(), vec!["edit:final", "delete"]);
    }

    #[tokio::test]
    async fn rejected_recovery_post_is_not_retried() {
        let adapter = Arc::new(RecordingAdapter::new());
        adapter.push_edit(rejected());
        adapter.push_delete(WriteOutcome::Delivered { message_id: None });
        adapter.push_send(rejected());
        adapter.push_send(delivered("must-not-send"));
        let erased: Arc<dyn ChatAdapter> = adapter.clone();

        let result =
            finalize_edit_placeholder(&erased, &channel(), &placeholder(), &["final".into()]).await;

        assert_eq!(result, ProgressiveDelivery::rejected());
        assert_eq!(adapter.events(), vec!["edit:final", "delete", "send:final"]);
    }

    #[tokio::test]
    async fn unknown_recovery_post_is_not_retried() {
        let adapter = Arc::new(RecordingAdapter::new());
        adapter.push_edit(rejected());
        adapter.push_delete(WriteOutcome::Delivered { message_id: None });
        adapter.push_send(unknown());
        adapter.push_send(delivered("must-not-send"));
        let erased: Arc<dyn ChatAdapter> = adapter.clone();

        let result =
            finalize_edit_placeholder(&erased, &channel(), &placeholder(), &["final".into()]).await;

        assert_eq!(result, ProgressiveDelivery::unknown());
        assert_eq!(adapter.events(), vec!["edit:final", "delete", "send:final"]);
    }

    #[tokio::test]
    async fn rejected_delete_then_failed_recovery_post_is_not_retried() {
        for (recovery, expected) in [
            (rejected(), ProgressiveDelivery::rejected()),
            (unknown(), ProgressiveDelivery::unknown()),
        ] {
            let adapter = Arc::new(RecordingAdapter::new());
            adapter.push_edit(rejected());
            adapter.push_delete(rejected());
            adapter.push_send(recovery);
            adapter.push_send(delivered("must-not-send"));
            let erased: Arc<dyn ChatAdapter> = adapter.clone();

            let result =
                finalize_edit_placeholder(&erased, &channel(), &placeholder(), &["final".into()])
                    .await;

            assert_eq!(result, expected);
            assert_eq!(adapter.events(), vec!["edit:final", "delete", "send:final"]);
        }
    }

    #[tokio::test]
    async fn rejected_overflow_stops_later_chunks() {
        let adapter = Arc::new(RecordingAdapter::new());
        adapter.push_edit(WriteOutcome::Delivered { message_id: None });
        adapter.push_send(rejected());
        adapter.push_send(delivered("must-not-send"));
        let erased: Arc<dyn ChatAdapter> = adapter.clone();

        let result = finalize_edit_placeholder(
            &erased,
            &channel(),
            &placeholder(),
            &["first".into(), "second".into(), "third".into()],
        )
        .await;

        assert_eq!(result, ProgressiveDelivery::rejected());
        assert_eq!(adapter.events(), vec!["edit:first", "send:second"]);
    }

    #[tokio::test]
    async fn unknown_overflow_stops_later_chunks() {
        let adapter = Arc::new(RecordingAdapter::new());
        adapter.push_edit(WriteOutcome::Delivered { message_id: None });
        adapter.push_send(unknown());
        adapter.push_send(delivered("must-not-send"));
        let erased: Arc<dyn ChatAdapter> = adapter.clone();

        let result = finalize_edit_placeholder(
            &erased,
            &channel(),
            &placeholder(),
            &["first".into(), "second".into(), "third".into()],
        )
        .await;

        assert_eq!(result, ProgressiveDelivery::unknown());
        assert_eq!(adapter.events(), vec!["edit:first", "send:second"]);
    }

    #[tokio::test]
    async fn explicit_reply_rejection_preserves_placeholder() {
        let adapter = Arc::new(RecordingAdapter::new());
        adapter.push_reply(rejected());
        let erased: Arc<dyn ChatAdapter> = adapter.clone();

        let result = finalize_explicit_reply(
            &erased,
            &channel(),
            &placeholder(),
            "quoted",
            &["final".into()],
        )
        .await;

        assert_eq!(result, ProgressiveDelivery::rejected());
        assert_eq!(adapter.events(), vec!["reply:final"]);
    }

    #[tokio::test]
    async fn explicit_reply_unknown_preserves_placeholder() {
        let adapter = Arc::new(RecordingAdapter::new());
        adapter.push_reply(unknown());
        let erased: Arc<dyn ChatAdapter> = adapter.clone();

        let result = finalize_explicit_reply(
            &erased,
            &channel(),
            &placeholder(),
            "quoted",
            &["final".into()],
        )
        .await;

        assert_eq!(result, ProgressiveDelivery::unknown());
        assert_eq!(adapter.events(), vec!["reply:final"]);
    }

    #[tokio::test]
    async fn explicit_reply_overflow_failure_preserves_placeholder() {
        for (overflow, expected) in [
            (rejected(), ProgressiveDelivery::rejected()),
            (unknown(), ProgressiveDelivery::unknown()),
        ] {
            let adapter = Arc::new(RecordingAdapter::new());
            adapter.push_reply(delivered("reply"));
            adapter.push_send(overflow);
            adapter.push_send(delivered("must-not-send"));
            let erased: Arc<dyn ChatAdapter> = adapter.clone();

            let result = finalize_explicit_reply(
                &erased,
                &channel(),
                &placeholder(),
                "quoted",
                &["first".into(), "second".into(), "third".into()],
            )
            .await;

            assert_eq!(result, expected);
            assert_eq!(adapter.events(), vec!["reply:first", "send:second"]);
        }
    }

    #[tokio::test]
    async fn explicit_reply_deletes_only_after_all_chunks_deliver() {
        let adapter = Arc::new(RecordingAdapter::new());
        adapter.push_reply(delivered("reply"));
        adapter.push_send(delivered("overflow"));
        adapter.push_delete(WriteOutcome::Delivered { message_id: None });
        let erased: Arc<dyn ChatAdapter> = adapter.clone();

        let result = finalize_explicit_reply(
            &erased,
            &channel(),
            &placeholder(),
            "quoted",
            &["first".into(), "second".into()],
        )
        .await;

        assert_eq!(result, ProgressiveDelivery::default());
        assert_eq!(
            adapter.events(),
            vec!["reply:first", "send:second", "delete"]
        );
    }

    #[tokio::test]
    async fn explicit_reply_cleanup_failure_does_not_retry_or_fail_content() {
        for cleanup in [rejected(), unknown()] {
            let adapter = Arc::new(RecordingAdapter::new());
            adapter.push_reply(delivered("reply"));
            adapter.push_delete(cleanup);
            adapter.push_delete(WriteOutcome::Delivered { message_id: None });
            let erased: Arc<dyn ChatAdapter> = adapter.clone();

            let result = finalize_explicit_reply(
                &erased,
                &channel(),
                &placeholder(),
                "quoted",
                &["final".into()],
            )
            .await;

            assert_eq!(result, ProgressiveDelivery::default());
            assert_eq!(adapter.events(), vec!["reply:final", "delete"]);
        }
    }
}

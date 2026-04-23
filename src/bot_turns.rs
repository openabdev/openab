//! Per-thread bot turn tracking for runaway-loop prevention.
//!
//! Shared between Discord and Slack adapters so both platforms apply the same
//! soft/hard limit semantics. Both counters reset on a human message in the
//! thread. Runs before self-check so a bot's own messages count too — this
//! means `soft_limit=20` caps the *total* bot messages in a thread, not per-bot.

use std::collections::HashMap;

/// Absolute per-thread cap on consecutive bot turns without human intervention.
/// A human message resets both soft and hard counters to 0, allowing bots to
/// resume. This is *not* a lifetime total — it guards against runaway loops
/// between human resets.
pub const HARD_BOT_TURN_LIMIT: u32 = 100;

#[derive(Debug, PartialEq, Eq)]
pub enum TurnResult {
    /// Counter below limits — continue normally.
    Ok,
    /// Counter == soft_limit — warn once, then stop.
    SoftLimit(u32),
    /// Counter > soft_limit — silently stop (already warned).
    Throttled,
    /// Counter == HARD_BOT_TURN_LIMIT — warn once, then stop.
    HardLimit,
    /// Counter > HARD_BOT_TURN_LIMIT — silently stop (already warned).
    Stopped,
}

pub struct BotTurnTracker {
    soft_limit: u32,
    counts: HashMap<String, (u32, u32)>,
}

impl BotTurnTracker {
    pub fn new(soft_limit: u32) -> Self {
        Self { soft_limit, counts: HashMap::new() }
    }

    pub fn on_bot_message(&mut self, thread_id: &str) -> TurnResult {
        let (soft, hard) = self.counts.entry(thread_id.to_string()).or_insert((0, 0));
        *soft += 1;
        *hard += 1;
        if *hard > HARD_BOT_TURN_LIMIT {
            TurnResult::Stopped
        } else if *hard == HARD_BOT_TURN_LIMIT {
            TurnResult::HardLimit
        } else if *soft > self.soft_limit {
            TurnResult::Throttled
        } else if *soft == self.soft_limit {
            TurnResult::SoftLimit(*soft)
        } else {
            TurnResult::Ok
        }
    }

    pub fn on_human_message(&mut self, thread_id: &str) {
        if let Some((soft, hard)) = self.counts.get_mut(thread_id) {
            *soft = 0;
            *hard = 0;
        }
    }

    /// High-level decision for a bot message: increments the counter and
    /// returns what the adapter should do. Collapses the warn-once semantics
    /// and user-facing message formatting so Discord/Slack (and future adapters)
    /// don't duplicate the match.
    pub fn classify_bot_message(&mut self, thread_id: &str) -> TurnAction {
        match self.on_bot_message(thread_id) {
            TurnResult::Ok => TurnAction::Continue,
            TurnResult::SoftLimit(n) => TurnAction::WarnAndStop {
                severity: TurnSeverity::Soft,
                turns: n,
                user_message: format!(
                    "⚠️ Bot turn limit reached ({n}/{soft}). \
                     A human must reply in this thread to continue bot-to-bot conversation.",
                    soft = self.soft_limit,
                ),
            },
            TurnResult::HardLimit => TurnAction::WarnAndStop {
                severity: TurnSeverity::Hard,
                turns: HARD_BOT_TURN_LIMIT,
                user_message: format!(
                    "🛑 Hard bot turn limit reached ({HARD_BOT_TURN_LIMIT}). \
                     A human must reply to continue."
                ),
            },
            TurnResult::Throttled | TurnResult::Stopped => TurnAction::SilentStop,
        }
    }
}

/// Log severity hint for `TurnAction::WarnAndStop`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum TurnSeverity {
    /// Soft limit — typically logged at `info!`.
    Soft,
    /// Hard absolute cap — typically logged at `warn!`.
    Hard,
}

/// High-level action for a bot message after calling
/// [`BotTurnTracker::classify_bot_message`].
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum TurnAction {
    /// Safe to continue processing this bot message.
    Continue,
    /// Stop processing; if the message did not come from our own bot, the
    /// caller should post `user_message` to the thread so humans see why
    /// the bot went quiet. `turns` is the counter value at the warning
    /// point — useful as a structured log field.
    WarnAndStop {
        severity: TurnSeverity,
        turns: u32,
        user_message: String,
    },
    /// Stop processing silently — the warning was already sent on a previous
    /// turn; further warnings would spam the thread.
    SilentStop,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_turns_increment() {
        let mut t = BotTurnTracker::new(5);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
    }

    #[test]
    fn soft_limit_triggers() {
        let mut t = BotTurnTracker::new(3);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::SoftLimit(3));
    }

    #[test]
    fn human_resets_both_counters() {
        let mut t = BotTurnTracker::new(3);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        t.on_human_message("t1");
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::SoftLimit(3));
    }

    #[test]
    fn hard_limit_triggers() {
        let mut t = BotTurnTracker::new(HARD_BOT_TURN_LIMIT + 1);
        for _ in 0..HARD_BOT_TURN_LIMIT - 1 {
            assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        }
        assert_eq!(t.on_bot_message("t1"), TurnResult::HardLimit);
    }

    #[test]
    fn hard_limit_resets_on_human() {
        let mut t = BotTurnTracker::new(HARD_BOT_TURN_LIMIT + 1);
        for _ in 0..HARD_BOT_TURN_LIMIT - 1 {
            assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        }
        t.on_human_message("t1");
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
    }

    #[test]
    fn hard_before_soft_when_equal() {
        let mut t = BotTurnTracker::new(HARD_BOT_TURN_LIMIT);
        for _ in 0..HARD_BOT_TURN_LIMIT - 1 {
            assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        }
        assert_eq!(t.on_bot_message("t1"), TurnResult::HardLimit);
    }

    #[test]
    fn threads_are_independent() {
        let mut t = BotTurnTracker::new(3);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::SoftLimit(3));
        assert_eq!(t.on_bot_message("t2"), TurnResult::Ok);
    }

    #[test]
    fn human_on_unknown_thread_is_noop() {
        let mut t = BotTurnTracker::new(5);
        t.on_human_message("unknown");
    }

    #[test]
    fn two_bot_pingpong_hits_soft_limit() {
        let mut t = BotTurnTracker::new(20);
        for i in 1..20 {
            assert_eq!(t.on_bot_message("t1"), TurnResult::Ok, "turn {i}");
        }
        assert_eq!(t.on_bot_message("t1"), TurnResult::SoftLimit(20));
    }

    #[test]
    fn two_bot_pingpong_human_resets() {
        let mut t = BotTurnTracker::new(20);
        for _ in 0..15 {
            assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        }
        t.on_human_message("t1");
        for _ in 0..15 {
            assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        }
        for _ in 0..4 {
            assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        }
        assert_eq!(t.on_bot_message("t1"), TurnResult::SoftLimit(20));
    }

    #[test]
    fn soft_limit_warn_once_semantics() {
        let mut t = BotTurnTracker::new(20);
        for _ in 0..19 {
            assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        }
        assert_eq!(t.on_bot_message("t1"), TurnResult::SoftLimit(20));
        assert_eq!(t.on_bot_message("t1"), TurnResult::Throttled);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Throttled);
    }

    #[test]
    fn hard_limit_warn_once_semantics() {
        let mut t = BotTurnTracker::new(HARD_BOT_TURN_LIMIT + 1);
        for _ in 0..HARD_BOT_TURN_LIMIT - 1 {
            assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        }
        assert_eq!(t.on_bot_message("t1"), TurnResult::HardLimit);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Stopped);
    }

    // System messages (thread created, pin, etc.) must not reset the counter.
    // Filtering happens at the call site; this verifies the counter stays put
    // when on_human_message is never called. Regression for openabdev/openab#497.
    #[test]
    fn system_message_does_not_reset_counter() {
        let mut t = BotTurnTracker::new(3);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::Ok);
        assert_eq!(t.on_bot_message("t1"), TurnResult::SoftLimit(3));
    }

    #[test]
    fn classify_returns_continue_under_limits() {
        let mut t = BotTurnTracker::new(5);
        assert_eq!(t.classify_bot_message("t1"), TurnAction::Continue);
    }

    #[test]
    fn classify_returns_warn_and_stop_on_soft_limit() {
        let mut t = BotTurnTracker::new(3);
        let _ = t.classify_bot_message("t1");
        let _ = t.classify_bot_message("t1");
        assert_eq!(
            t.classify_bot_message("t1"),
            TurnAction::WarnAndStop {
                severity: TurnSeverity::Soft,
                turns: 3,
                user_message: "⚠️ Bot turn limit reached (3/3). \
                               A human must reply in this thread to continue bot-to-bot conversation."
                    .to_string(),
            },
        );
    }

    #[test]
    fn classify_returns_silent_stop_past_soft_limit() {
        let mut t = BotTurnTracker::new(2);
        let _ = t.classify_bot_message("t1");
        let _ = t.classify_bot_message("t1");
        assert_eq!(t.classify_bot_message("t1"), TurnAction::SilentStop);
        assert_eq!(t.classify_bot_message("t1"), TurnAction::SilentStop);
    }

    #[test]
    fn classify_returns_warn_and_stop_on_hard_limit() {
        let mut t = BotTurnTracker::new(HARD_BOT_TURN_LIMIT + 1);
        for _ in 0..HARD_BOT_TURN_LIMIT - 1 {
            let _ = t.classify_bot_message("t1");
        }
        assert_eq!(
            t.classify_bot_message("t1"),
            TurnAction::WarnAndStop {
                severity: TurnSeverity::Hard,
                turns: HARD_BOT_TURN_LIMIT,
                user_message: format!(
                    "🛑 Hard bot turn limit reached ({HARD_BOT_TURN_LIMIT}). \
                     A human must reply to continue."
                ),
            },
        );
        assert_eq!(t.classify_bot_message("t1"), TurnAction::SilentStop);
    }

    #[test]
    fn classify_is_per_thread_independent() {
        let mut t = BotTurnTracker::new(2);
        assert_eq!(t.classify_bot_message("t1"), TurnAction::Continue);
        assert!(matches!(
            t.classify_bot_message("t1"),
            TurnAction::WarnAndStop { severity: TurnSeverity::Soft, .. },
        ));
        assert_eq!(t.classify_bot_message("t2"), TurnAction::Continue);
        assert!(matches!(
            t.classify_bot_message("t2"),
            TurnAction::WarnAndStop { severity: TurnSeverity::Soft, .. },
        ));
    }

    // End-to-end: human message must fully reset classify behavior on the
    // same thread, including unlocking new `Continue` responses.
    #[test]
    fn classify_resumes_after_human_message() {
        let mut t = BotTurnTracker::new(2);
        let _ = t.classify_bot_message("t1"); // Continue
        assert!(matches!(
            t.classify_bot_message("t1"),
            TurnAction::WarnAndStop { .. },
        ));
        // Without a human message, the next classify is silent.
        assert_eq!(t.classify_bot_message("t1"), TurnAction::SilentStop);
        // Human resets — classify starts at Continue again.
        t.on_human_message("t1");
        assert_eq!(t.classify_bot_message("t1"), TurnAction::Continue);
        assert!(matches!(
            t.classify_bot_message("t1"),
            TurnAction::WarnAndStop { severity: TurnSeverity::Soft, turns: 2, .. },
        ));
    }
}

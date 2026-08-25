//! Event Triage — decide whether an unsolicited event is worth waking the agent
//! for (ADR: `docs/adr/structured-delivery.md` §7, Phase 3).
//!
//! # Two layers of quiet
//!
//! An agent that watches a mailbox or a calendar sees far more events than are
//! worth a message. Two independent layers keep it from becoming a nuisance:
//!
//! 1. **This layer** — cheap, deterministic, and decided *before* any model call:
//!    duplicate delivery, quiet hours, a per-conversation cooldown, a daily cap.
//! 2. **The agent** — expensive and contextual: with the turn envelope enabled
//!    it can answer `next: "silent"` after actually reading the event.
//!
//! Layer 2 alone would work, and would cost an LLM call for every routine
//! notification that arrives at 3am. Layer 1 exists so layer 2 is only consulted
//! when a reply is plausible in the first place.
//!
//! # What must never be triaged
//!
//! **Only unsolicited events pass through here.** A message a human actually
//! sent is dispatched no matter the hour: quiet hours that swallow a user's
//! question are not a feature, they are an outage. The caller enforces this by
//! consulting [`TriageState::admit`] only for events explicitly flagged
//! proactive.
//!
//! # Observability
//!
//! A suppressed event still produces a structured record — [`SuppressReason`] is
//! the vocabulary. "The agent said nothing" and "the broker never asked it" look
//! identical from the outside, and only the log can tell them apart.

use chrono::{DateTime, NaiveDate, Timelike, Utc};
use chrono_tz::Tz;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// A daily quiet window, as minutes since local midnight.
///
/// A window may wrap midnight (`22:00-08:00`), which is the common case — so
/// containment is not a simple range check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuietHours {
    start_min: u32,
    end_min: u32,
}

impl QuietHours {
    /// Parse `"HH:MM-HH:MM"`. The window is half-open: the start minute is
    /// quiet, the end minute is not, so `22:00-08:00` ends at 08:00 sharp.
    ///
    /// `start == end` is an empty window (never quiet), not a 24-hour one —
    /// a config that silences the agent forever should have to say `enabled = false`.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let (start, end) = spec
            .split_once('-')
            .ok_or_else(|| format!("quiet_hours must be `HH:MM-HH:MM`, got {spec:?}"))?;
        Ok(Self {
            start_min: parse_hh_mm(start.trim())?,
            end_min: parse_hh_mm(end.trim())?,
        })
    }

    /// Whether `minutes` (since local midnight) falls inside the window.
    pub fn contains(&self, minutes: u32) -> bool {
        if self.start_min == self.end_min {
            false
        } else if self.start_min < self.end_min {
            minutes >= self.start_min && minutes < self.end_min
        } else {
            // Wraps midnight: quiet from start to 24:00, and 00:00 to end.
            minutes >= self.start_min || minutes < self.end_min
        }
    }
}

fn parse_hh_mm(value: &str) -> Result<u32, String> {
    let (h, m) = value
        .split_once(':')
        .ok_or_else(|| format!("expected `HH:MM`, got {value:?}"))?;
    let h: u32 = h
        .parse()
        .map_err(|_| format!("invalid hour in {value:?}"))?;
    let m: u32 = m
        .parse()
        .map_err(|_| format!("invalid minute in {value:?}"))?;
    if h > 23 {
        return Err(format!("hour out of range in {value:?}"));
    }
    if m > 59 {
        return Err(format!("minute out of range in {value:?}"));
    }
    Ok(h * 60 + m)
}

/// Triage settings with the string forms already parsed and validated.
///
/// Mirrors [`crate::trust::TrustConfig`]: the TOML-facing struct lives in
/// `config.rs`, and this is what the runtime actually consults.
#[derive(Debug, Clone)]
pub struct TriageSettings {
    /// When false every proactive event is admitted — the pre-Phase-3 behavior.
    pub enabled: bool,
    pub quiet_hours: Option<QuietHours>,
    pub timezone: Tz,
    /// Minimum gap between two proactive wakes on one conversation. `0` disables.
    pub cooldown_secs: u64,
    /// Maximum proactive wakes per conversation per local day. `0` disables.
    pub daily_cap: u32,
    /// How long a delivered `event_id` is remembered for duplicate suppression.
    pub dedupe_window_secs: u64,
}

impl Default for TriageSettings {
    /// Disabled — proactive events pass straight through, as before Phase 3.
    fn default() -> Self {
        Self {
            enabled: false,
            quiet_hours: None,
            timezone: Tz::UTC,
            cooldown_secs: 0,
            daily_cap: 0,
            dedupe_window_secs: 0,
        }
    }
}

/// Why a proactive event did not reach the agent.
///
/// `#[non_exhaustive]` because later phases add reasons (an ignored-alert
/// history, a per-source mute); callers must include a `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SuppressReason {
    /// This `event_id` was already admitted inside the dedupe window.
    Duplicate,
    /// Local time falls inside the configured quiet window.
    QuietHours,
    /// Too soon after the previous proactive wake on this conversation.
    Cooldown { remaining_secs: u64 },
    /// This conversation already used its allowance for the local day.
    DailyCap { cap: u32 },
}

impl SuppressReason {
    /// Stable machine-readable tag for logs and metrics. Kept separate from
    /// `Display` so a message reword cannot silently break a dashboard.
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Duplicate => "duplicate",
            Self::QuietHours => "quiet_hours",
            Self::Cooldown { .. } => "cooldown",
            Self::DailyCap { .. } => "daily_cap",
        }
    }
}

impl std::fmt::Display for SuppressReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Duplicate => write!(f, "duplicate event"),
            Self::QuietHours => write!(f, "inside quiet hours"),
            Self::Cooldown { remaining_secs } => {
                write!(f, "cooldown ({remaining_secs}s remaining)")
            }
            Self::DailyCap { cap } => write!(f, "daily cap of {cap} reached"),
        }
    }
}

/// Outcome of triaging one proactive event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Dispatch it — the agent decides for itself whether to speak.
    Wake,
    /// Drop it. The agent is never told the event happened.
    Suppress(SuppressReason),
}

impl Decision {
    pub fn is_wake(&self) -> bool {
        matches!(self, Self::Wake)
    }
}

/// Per-conversation triage bookkeeping.
#[derive(Debug, Default)]
struct ConversationState {
    /// Admitted-or-seen event ids with their arrival time, oldest first.
    /// Every evaluated event is recorded regardless of outcome: an event
    /// suppressed by quiet hours must still be recognised as a duplicate when
    /// the same delivery is retried after the window closes.
    seen: VecDeque<(String, DateTime<Utc>)>,
    last_wake: Option<DateTime<Utc>>,
    /// Local day the counter belongs to; a new day resets `woke_today`.
    counter_day: Option<NaiveDate>,
    woke_today: u32,
}

/// Shared, cheap-to-clone triage bookkeeping.
///
/// In-memory only, matching the other cross-turn caches in this crate: a
/// restart forgets the cooldown and the day's count. That is the safe direction
/// to be wrong in — a restart may allow one extra proactive message, never
/// silence a needed one. Durable counters belong to the agent runtime, which
/// owns the user-facing state anyway.
#[derive(Clone, Default)]
pub struct TriageState {
    conversations: Arc<Mutex<HashMap<String, ConversationState>>>,
}

impl TriageState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Triage one proactive event, recording the outcome.
    ///
    /// Evaluation and bookkeeping share one lock: two events arriving together
    /// must not both pass a cooldown that only allows one.
    ///
    /// `key` scopes the counters — pass the conversation the message would land
    /// in (`platform:channel_id`), so a busy work channel cannot exhaust a
    /// private DM's allowance.
    pub fn admit(
        &self,
        key: &str,
        event_id: &str,
        settings: &TriageSettings,
        now: DateTime<Utc>,
    ) -> Decision {
        if !settings.enabled {
            return Decision::Wake;
        }

        let mut conversations = self.conversations.lock().unwrap_or_else(|e| e.into_inner());
        let state = conversations.entry(key.to_string()).or_default();

        // 1. Duplicate delivery. Cheapest and most certain, so it runs first —
        //    and it is recorded even when a later rule suppresses the event.
        if settings.dedupe_window_secs > 0 {
            let cutoff = now - chrono::Duration::seconds(settings.dedupe_window_secs as i64);
            while state.seen.front().is_some_and(|(_, at)| *at < cutoff) {
                state.seen.pop_front();
            }
            if !event_id.is_empty() && state.seen.iter().any(|(id, _)| id == event_id) {
                return Decision::Suppress(SuppressReason::Duplicate);
            }
            if !event_id.is_empty() {
                state.seen.push_back((event_id.to_string(), now));
            }
        }

        let local = now.with_timezone(&settings.timezone);

        // 2. Quiet hours.
        if let Some(window) = settings.quiet_hours {
            if window.contains(local.hour() * 60 + local.minute()) {
                return Decision::Suppress(SuppressReason::QuietHours);
            }
        }

        // 3. Cooldown since the last event that actually woke the agent.
        if settings.cooldown_secs > 0 {
            if let Some(last) = state.last_wake {
                let elapsed = now.signed_duration_since(last).num_seconds();
                // A negative elapsed means the clock moved backwards (NTP step);
                // treat it as "no time has passed" rather than as a huge gap.
                let elapsed = elapsed.max(0) as u64;
                if elapsed < settings.cooldown_secs {
                    return Decision::Suppress(SuppressReason::Cooldown {
                        remaining_secs: settings.cooldown_secs - elapsed,
                    });
                }
            }
        }

        // 4. Daily cap, counted in the configured timezone's day.
        let today = local.date_naive();
        if state.counter_day != Some(today) {
            state.counter_day = Some(today);
            state.woke_today = 0;
        }
        if settings.daily_cap > 0 && state.woke_today >= settings.daily_cap {
            return Decision::Suppress(SuppressReason::DailyCap {
                cap: settings.daily_cap,
            });
        }

        // Admitted: only now does it count against the cooldown and the cap.
        // A suppressed event must not consume the allowance it was denied.
        state.last_wake = Some(now);
        state.woke_today += 1;
        Decision::Wake
    }

    /// Number of tracked conversations (diagnostics / tests).
    pub fn tracked_conversations(&self) -> usize {
        self.conversations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Drop conversations whose last activity predates `cutoff`, so a long-lived
    /// broker does not accumulate state for channels that went quiet. Called
    /// from the same sweep that expires idle sessions.
    pub fn sweep(&self, cutoff: DateTime<Utc>) {
        let mut conversations = self.conversations.lock().unwrap_or_else(|e| e.into_inner());
        conversations.retain(|_, state| {
            let newest_seen = state.seen.back().map(|(_, at)| *at);
            let newest = match (state.last_wake, newest_seen) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            newest.is_some_and(|at| at >= cutoff)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn settings() -> TriageSettings {
        TriageSettings {
            enabled: true,
            ..TriageSettings::default()
        }
    }

    // --- quiet hours parsing ---

    #[test]
    fn quiet_hours_parses_a_same_day_window() {
        let w = QuietHours::parse("09:00-17:30").unwrap();
        assert!(!w.contains(8 * 60 + 59));
        assert!(w.contains(9 * 60));
        assert!(w.contains(17 * 60 + 29));
        assert!(!w.contains(17 * 60 + 30), "end minute is exclusive");
    }

    #[test]
    fn quiet_hours_wraps_midnight() {
        let w = QuietHours::parse("22:00-08:00").unwrap();
        assert!(w.contains(23 * 60));
        assert!(w.contains(0), "midnight is inside a wrapping window");
        assert!(w.contains(7 * 60 + 59));
        assert!(!w.contains(8 * 60));
        assert!(!w.contains(12 * 60));
    }

    #[test]
    fn quiet_hours_equal_bounds_is_never_quiet() {
        // Silencing the agent forever must require `enabled = false`.
        let w = QuietHours::parse("00:00-00:00").unwrap();
        for m in [0, 1, 720, 1439] {
            assert!(!w.contains(m));
        }
    }

    #[test]
    fn quiet_hours_rejects_malformed_specs() {
        for bad in [
            "22:00",
            "22:00/08:00",
            "25:00-08:00",
            "22:60-08:00",
            "x:y-1:2",
        ] {
            assert!(QuietHours::parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    // --- the disabled default ---

    #[test]
    fn disabled_triage_admits_everything() {
        let state = TriageState::new();
        let cfg = TriageSettings::default();
        for i in 0..10 {
            let d = state.admit("c1", &format!("e{i}"), &cfg, at("2026-08-25T03:00:00Z"));
            assert_eq!(d, Decision::Wake);
        }
        // Even a repeat id: with triage off, nothing is tracked at all.
        assert_eq!(
            state.admit("c1", "e0", &cfg, at("2026-08-25T03:00:00Z")),
            Decision::Wake
        );
    }

    // --- dedupe ---

    #[test]
    fn duplicate_event_id_is_suppressed_within_the_window() {
        let state = TriageState::new();
        let cfg = TriageSettings {
            dedupe_window_secs: 3600,
            ..settings()
        };
        let t = at("2026-08-25T12:00:00Z");
        assert_eq!(state.admit("c1", "evt_1", &cfg, t), Decision::Wake);
        assert_eq!(
            state.admit("c1", "evt_1", &cfg, t + chrono::Duration::seconds(30)),
            Decision::Suppress(SuppressReason::Duplicate)
        );
    }

    #[test]
    fn duplicate_expires_with_the_window() {
        let state = TriageState::new();
        let cfg = TriageSettings {
            dedupe_window_secs: 60,
            ..settings()
        };
        let t = at("2026-08-25T12:00:00Z");
        state.admit("c1", "evt_1", &cfg, t);
        assert_eq!(
            state.admit("c1", "evt_1", &cfg, t + chrono::Duration::seconds(61)),
            Decision::Wake
        );
    }

    #[test]
    fn a_quiet_hours_suppression_still_registers_the_id() {
        // Otherwise a redelivery after the window closes reads as a new event.
        let state = TriageState::new();
        let cfg = TriageSettings {
            quiet_hours: Some(QuietHours::parse("22:00-08:00").unwrap()),
            dedupe_window_secs: 86_400,
            ..settings()
        };
        assert_eq!(
            state.admit("c1", "evt_1", &cfg, at("2026-08-25T23:00:00Z")),
            Decision::Suppress(SuppressReason::QuietHours)
        );
        assert_eq!(
            state.admit("c1", "evt_1", &cfg, at("2026-08-26T09:00:00Z")),
            Decision::Suppress(SuppressReason::Duplicate),
            "the same delivery must not sneak through once the window closes"
        );
    }

    #[test]
    fn an_empty_event_id_is_never_deduped() {
        let state = TriageState::new();
        let cfg = TriageSettings {
            dedupe_window_secs: 3600,
            ..settings()
        };
        let t = at("2026-08-25T12:00:00Z");
        assert_eq!(state.admit("c1", "", &cfg, t), Decision::Wake);
        // A second id-less event is judged on its own merits, not collapsed
        // into the first.
        assert_eq!(state.admit("c1", "", &cfg, t), Decision::Wake);
    }

    // --- quiet hours ---

    #[test]
    fn quiet_hours_are_evaluated_in_the_configured_timezone() {
        let state = TriageState::new();
        let cfg = TriageSettings {
            quiet_hours: Some(QuietHours::parse("22:00-08:00").unwrap()),
            timezone: "Asia/Taipei".parse().unwrap(),
            ..settings()
        };
        // 16:00Z is midnight in Taipei (UTC+8) — quiet.
        assert_eq!(
            state.admit("c1", "e1", &cfg, at("2026-08-25T16:00:00Z")),
            Decision::Suppress(SuppressReason::QuietHours)
        );
        // 04:00Z is noon in Taipei — not quiet.
        assert_eq!(
            state.admit("c1", "e2", &cfg, at("2026-08-25T04:00:00Z")),
            Decision::Wake
        );
    }

    // --- cooldown ---

    #[test]
    fn cooldown_blocks_a_second_wake_and_reports_the_remainder() {
        let state = TriageState::new();
        let cfg = TriageSettings {
            cooldown_secs: 900,
            ..settings()
        };
        let t = at("2026-08-25T12:00:00Z");
        assert_eq!(state.admit("c1", "e1", &cfg, t), Decision::Wake);
        assert_eq!(
            state.admit("c1", "e2", &cfg, t + chrono::Duration::seconds(300)),
            Decision::Suppress(SuppressReason::Cooldown {
                remaining_secs: 600
            })
        );
        assert_eq!(
            state.admit("c1", "e3", &cfg, t + chrono::Duration::seconds(900)),
            Decision::Wake
        );
    }

    #[test]
    fn a_suppressed_event_does_not_restart_the_cooldown() {
        let state = TriageState::new();
        let cfg = TriageSettings {
            cooldown_secs: 600,
            ..settings()
        };
        let t = at("2026-08-25T12:00:00Z");
        state.admit("c1", "e1", &cfg, t);
        // Denied at t+300; must not push the next opening out to t+900.
        state.admit("c1", "e2", &cfg, t + chrono::Duration::seconds(300));
        assert_eq!(
            state.admit("c1", "e3", &cfg, t + chrono::Duration::seconds(600)),
            Decision::Wake
        );
    }

    #[test]
    fn a_backwards_clock_does_not_open_the_cooldown() {
        let state = TriageState::new();
        let cfg = TriageSettings {
            cooldown_secs: 600,
            ..settings()
        };
        let t = at("2026-08-25T12:00:00Z");
        state.admit("c1", "e1", &cfg, t);
        // NTP steps the clock back: elapsed is negative, not "ages ago".
        assert!(matches!(
            state.admit("c1", "e2", &cfg, t - chrono::Duration::seconds(120)),
            Decision::Suppress(SuppressReason::Cooldown { .. })
        ));
    }

    #[test]
    fn cooldown_is_per_conversation() {
        let state = TriageState::new();
        let cfg = TriageSettings {
            cooldown_secs: 900,
            ..settings()
        };
        let t = at("2026-08-25T12:00:00Z");
        assert_eq!(state.admit("c1", "e1", &cfg, t), Decision::Wake);
        assert_eq!(
            state.admit("c2", "e2", &cfg, t),
            Decision::Wake,
            "a busy channel must not exhaust another conversation's allowance"
        );
    }

    // --- daily cap ---

    #[test]
    fn daily_cap_stops_at_the_limit_and_resets_next_local_day() {
        let state = TriageState::new();
        let cfg = TriageSettings {
            daily_cap: 2,
            ..settings()
        };
        let t = at("2026-08-25T10:00:00Z");
        assert_eq!(state.admit("c1", "e1", &cfg, t), Decision::Wake);
        assert_eq!(
            state.admit("c1", "e2", &cfg, t + chrono::Duration::seconds(1)),
            Decision::Wake
        );
        assert_eq!(
            state.admit("c1", "e3", &cfg, t + chrono::Duration::seconds(2)),
            Decision::Suppress(SuppressReason::DailyCap { cap: 2 })
        );
        // Next day in the configured zone: allowance is back.
        assert_eq!(
            state.admit("c1", "e4", &cfg, at("2026-08-26T10:00:00Z")),
            Decision::Wake
        );
    }

    #[test]
    fn daily_cap_rolls_over_on_the_configured_timezone_day() {
        let state = TriageState::new();
        let cfg = TriageSettings {
            daily_cap: 1,
            timezone: "Asia/Taipei".parse().unwrap(),
            ..settings()
        };
        // 15:00Z = 23:00 Taipei on the 25th.
        assert_eq!(
            state.admit("c1", "e1", &cfg, at("2026-08-25T15:00:00Z")),
            Decision::Wake
        );
        // 17:00Z = 01:00 Taipei on the 26th — a new local day, even though it
        // is still the 25th in UTC.
        assert_eq!(
            state.admit("c1", "e2", &cfg, at("2026-08-25T17:00:00Z")),
            Decision::Wake
        );
    }

    #[test]
    fn zero_means_unlimited_for_both_caps() {
        let state = TriageState::new();
        let cfg = TriageSettings {
            cooldown_secs: 0,
            daily_cap: 0,
            ..settings()
        };
        let t = at("2026-08-25T12:00:00Z");
        for i in 0..20 {
            assert_eq!(state.admit("c1", &format!("e{i}"), &cfg, t), Decision::Wake);
        }
    }

    // --- ordering ---

    #[test]
    fn duplicate_is_reported_ahead_of_quiet_hours() {
        // Cheapest and most certain rule wins, so the log names the real cause.
        let state = TriageState::new();
        let cfg = TriageSettings {
            quiet_hours: Some(QuietHours::parse("22:00-08:00").unwrap()),
            dedupe_window_secs: 7200,
            ..settings()
        };
        // 21:30Z is outside the window, 22:30Z is inside it, and the two are
        // an hour apart — well within the dedupe window, so the duplicate rule
        // is the one that should fire.
        state.admit("c1", "e1", &cfg, at("2026-08-25T21:30:00Z"));
        assert_eq!(
            state.admit("c1", "e1", &cfg, at("2026-08-25T22:30:00Z")),
            Decision::Suppress(SuppressReason::Duplicate)
        );
    }

    #[test]
    fn suppress_reasons_have_stable_tags() {
        assert_eq!(SuppressReason::Duplicate.tag(), "duplicate");
        assert_eq!(SuppressReason::QuietHours.tag(), "quiet_hours");
        assert_eq!(
            SuppressReason::Cooldown { remaining_secs: 5 }.tag(),
            "cooldown"
        );
        assert_eq!(SuppressReason::DailyCap { cap: 3 }.tag(), "daily_cap");
    }

    // --- housekeeping ---

    #[test]
    fn sweep_drops_only_stale_conversations() {
        let state = TriageState::new();
        let cfg = TriageSettings {
            dedupe_window_secs: 3600,
            ..settings()
        };
        state.admit("old", "e1", &cfg, at("2026-08-20T12:00:00Z"));
        state.admit("fresh", "e2", &cfg, at("2026-08-25T12:00:00Z"));
        assert_eq!(state.tracked_conversations(), 2);
        state.sweep(at("2026-08-24T00:00:00Z"));
        assert_eq!(state.tracked_conversations(), 1);
        // The survivor keeps its history — sweeping is not a reset.
        assert_eq!(
            state.admit("fresh", "e2", &cfg, at("2026-08-25T12:30:00Z")),
            Decision::Suppress(SuppressReason::Duplicate)
        );
    }
}

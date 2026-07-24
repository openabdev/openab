//! Per-server circuit breaker (ADR §5.9).
//!
//! Design decisions (#966):
//! - Fixed cooldown 3-state breaker (Closed / Open / HalfOpen)
//! - Single consecutive-failure counter per server (transport-level only —
//!   JSON-RPC error responses and tool `isError: true` content do NOT count)
//! - Lazy / piggyback probe: after cooldown elapses the next call becomes
//!   the half-open probe (matches Hermes `tools/mcp_tool.py` lines 1868-1912
//!   and 2480-2510)
//!
//! ADR §5.9 mentions "3 fails in 30s" but Hermes itself tracks pure
//! consecutive failures with no time window — going Hermes-simple here.
//! Any success resets the counter.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Number of consecutive transport failures that trip the breaker.
pub const FAIL_THRESHOLD: u32 = 3;

/// Cooldown after the breaker opens before the next probe is allowed.
pub const COOLDOWN: Duration = Duration::from_secs(60);

/// Outcome of [`ServerBreaker::check`] — the call site uses this to decide
/// whether to short-circuit or proceed (and, if proceeding, whether the
/// upcoming call is a half-open probe).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Breaker is `Closed` — call goes through normally.
    Allow,
    /// Breaker is `HalfOpen` — cooldown elapsed, allow exactly one probe
    /// call. The next [`record_success`](ServerBreaker::record_success) or
    /// [`record_failure`](ServerBreaker::record_failure) decides the next
    /// state.
    AllowProbe,
    /// Breaker is `Open` — short-circuit the call with this hint to the
    /// caller / LLM.
    Reject { retry_in_secs: u64 },
}

#[derive(Debug, Default)]
struct Entry {
    consecutive_failures: u32,
    opened_at: Option<Instant>,
    /// When the in-flight half-open probe was handed out, if any. Replaces a
    /// bare `bool` so a probe whose caller never records an outcome (panicked
    /// or dropped mid-dial) can be aged out: once it is older than [`COOLDOWN`]
    /// it is treated as stale and a fresh probe is re-armed, rather than
    /// wedging the breaker half-open forever (#969 C6).
    probe_started_at: Option<Instant>,
}

/// Per-server circuit breaker state. Cheap to clone — wraps a `Mutex` so
/// callers can share via `Arc<ServerBreaker>` if they need cross-task
/// access without re-acquiring at the `McpRuntimeManager` level.
#[derive(Debug, Default)]
pub struct ServerBreaker {
    entries: Mutex<HashMap<String, Entry>>,
}

impl ServerBreaker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn check(&self, server: &str) -> Verdict {
        self.check_at(server, Instant::now())
    }

    /// Internal: parameterized check with an injectable clock so tests can
    /// fast-forward past [`COOLDOWN`] without `tokio::time::sleep`.
    fn check_at(&self, server: &str, now: Instant) -> Verdict {
        let mut entries = self.entries.lock().expect("breaker mutex poisoned");
        let Some(entry) = entries.get_mut(server) else {
            return Verdict::Allow;
        };
        if entry.consecutive_failures < FAIL_THRESHOLD {
            return Verdict::Allow;
        }
        let Some(opened_at) = entry.opened_at else {
            return Verdict::Allow;
        };
        let age = now.saturating_duration_since(opened_at);
        if age >= COOLDOWN {
            // Only one probe at a time — but a probe outstanding longer than
            // COOLDOWN is stale (its caller never recorded an outcome), so let
            // it lapse and re-arm a fresh probe instead of staying half-open
            // forever (#969 C6).
            if let Some(probe_started) = entry.probe_started_at {
                if now.saturating_duration_since(probe_started) < COOLDOWN {
                    return Verdict::Reject { retry_in_secs: 1 };
                }
            }
            entry.probe_started_at = Some(now);
            Verdict::AllowProbe
        } else {
            // Floor at 1s: returning 0 would render as "retry in 0s" to the
            // LLM/CLI, which reads as "retry now" and defeats the cooldown.
            let remaining = COOLDOWN.saturating_sub(age).as_secs().max(1);
            Verdict::Reject {
                retry_in_secs: remaining,
            }
        }
    }

    /// Reset the breaker for `server` — clears failure count and opened-at
    /// timestamp. Call on any unambiguous success (successful tool call,
    /// successful connect).
    pub fn record_success(&self, server: &str) {
        let mut entries = self.entries.lock().expect("breaker mutex poisoned");
        entries.remove(server);
    }

    /// Record a transport-level failure for `server`. When the count
    /// reaches [`FAIL_THRESHOLD`], stamps the opened-at timestamp so the
    /// cooldown clock starts (or re-starts, for half-open probe failures).
    pub fn record_failure(&self, server: &str) {
        self.record_failure_at(server, Instant::now());
    }

    fn record_failure_at(&self, server: &str, now: Instant) {
        let mut entries = self.entries.lock().expect("breaker mutex poisoned");
        let entry = entries.entry(server.to_string()).or_default();
        let was_probe = entry.probe_started_at.is_some();
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        entry.probe_started_at = None;
        if entry.consecutive_failures >= FAIL_THRESHOLD {
            // Stamp `opened_at` only on the Closed→Open transition (no
            // timestamp yet) or when a half-open probe just failed
            // (`was_probe`), which must re-arm the cooldown. A passive failure
            // while already Open must NOT re-stamp: doing so restarts the
            // cooldown clock on every failure so the breaker never reaches
            // half-open (#969 F1).
            if entry.opened_at.is_none() || was_probe {
                entry.opened_at = Some(now);
            }
        }
    }

    /// True while the breaker has tripped for `server` — it has reached
    /// [`FAIL_THRESHOLD`] consecutive failures and not yet been reset by a
    /// success (covers both the Open cooldown and the half-open probe window).
    /// Non-mutating, unlike [`check`](ServerBreaker::check) which arms a probe
    /// on the half-open transition; callers that only need to gate passive
    /// failure reporting (the ping loop) use this so they don't disturb the
    /// foreground probe state.
    pub fn is_tripped(&self, server: &str) -> bool {
        let entries = self.entries.lock().expect("breaker mutex poisoned");
        match entries.get(server) {
            Some(e) => e.consecutive_failures >= FAIL_THRESHOLD && e.opened_at.is_some(),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_server_allows() {
        let b = ServerBreaker::new();
        assert_eq!(b.check("foo"), Verdict::Allow);
    }

    #[test]
    fn under_threshold_allows() {
        let b = ServerBreaker::new();
        b.record_failure("foo");
        b.record_failure("foo");
        assert_eq!(b.check("foo"), Verdict::Allow);
    }

    #[test]
    fn threshold_opens_breaker() {
        let b = ServerBreaker::new();
        for _ in 0..FAIL_THRESHOLD {
            b.record_failure("foo");
        }
        match b.check("foo") {
            Verdict::Reject { retry_in_secs } => {
                assert!(retry_in_secs > 0 && retry_in_secs <= COOLDOWN.as_secs());
            }
            v => panic!("expected Reject, got {v:?}"),
        }
    }

    #[test]
    fn success_resets_count() {
        let b = ServerBreaker::new();
        b.record_failure("foo");
        b.record_failure("foo");
        b.record_success("foo");
        b.record_failure("foo");
        assert_eq!(b.check("foo"), Verdict::Allow);
    }

    #[test]
    fn cooldown_elapsed_allows_probe() {
        let b = ServerBreaker::new();
        let t0 = Instant::now();
        for _ in 0..FAIL_THRESHOLD {
            b.record_failure_at("foo", t0);
        }
        assert!(matches!(b.check_at("foo", t0), Verdict::Reject { .. }));
        let t1 = t0 + COOLDOWN + Duration::from_secs(1);
        assert_eq!(b.check_at("foo", t1), Verdict::AllowProbe);
    }

    #[test]
    fn only_one_probe_is_allowed_after_cooldown() {
        let b = ServerBreaker::new();
        let t0 = Instant::now();
        for _ in 0..FAIL_THRESHOLD {
            b.record_failure_at("foo", t0);
        }
        let t1 = t0 + COOLDOWN + Duration::from_secs(1);
        assert_eq!(b.check_at("foo", t1), Verdict::AllowProbe);
        assert_eq!(b.check_at("foo", t1), Verdict::Reject { retry_in_secs: 1 });
    }

    #[test]
    fn probe_failure_rearms_cooldown() {
        let b = ServerBreaker::new();
        let t0 = Instant::now();
        for _ in 0..FAIL_THRESHOLD {
            b.record_failure_at("foo", t0);
        }
        let t1 = t0 + COOLDOWN + Duration::from_secs(1);
        assert_eq!(b.check_at("foo", t1), Verdict::AllowProbe);
        b.record_failure_at("foo", t1);
        match b.check_at("foo", t1) {
            Verdict::Reject { retry_in_secs } => {
                assert!(retry_in_secs >= COOLDOWN.as_secs() - 1);
            }
            v => panic!("expected Reject after probe failure, got {v:?}"),
        }
    }

    #[test]
    fn probe_success_closes_breaker() {
        let b = ServerBreaker::new();
        let t0 = Instant::now();
        for _ in 0..FAIL_THRESHOLD {
            b.record_failure_at("foo", t0);
        }
        let t1 = t0 + COOLDOWN + Duration::from_secs(1);
        assert_eq!(b.check_at("foo", t1), Verdict::AllowProbe);
        b.record_success("foo");
        assert_eq!(b.check_at("foo", t1), Verdict::Allow);
    }

    #[test]
    fn per_server_isolation() {
        let b = ServerBreaker::new();
        for _ in 0..FAIL_THRESHOLD {
            b.record_failure("foo");
        }
        assert!(matches!(b.check("foo"), Verdict::Reject { .. }));
        assert_eq!(b.check("bar"), Verdict::Allow);
    }

    #[test]
    fn passive_failure_while_open_does_not_restamp_cooldown() {
        let b = ServerBreaker::new();
        let t0 = Instant::now();
        for _ in 0..FAIL_THRESHOLD {
            b.record_failure_at("foo", t0);
        }
        // A passive failure partway through the cooldown must not restart the
        // clock (it is not a half-open probe).
        let t_mid = t0 + Duration::from_secs(30);
        b.record_failure_at("foo", t_mid);
        // Cooldown is still measured from t0, so the probe is allowed here.
        // Had the passive failure re-stamped opened_at to t_mid, this would
        // still Reject.
        let t_after = t0 + COOLDOWN + Duration::from_secs(1);
        assert_eq!(b.check_at("foo", t_after), Verdict::AllowProbe);
    }

    #[test]
    fn is_tripped_tracks_open_state() {
        let b = ServerBreaker::new();
        assert!(!b.is_tripped("foo"));
        b.record_failure("foo");
        assert!(!b.is_tripped("foo"), "below threshold is not tripped");
        for _ in 1..FAIL_THRESHOLD {
            b.record_failure("foo");
        }
        assert!(b.is_tripped("foo"), "tripped once threshold reached");
        // Half-open window (cooldown elapsed) is still tripped.
        let t1 = Instant::now() + COOLDOWN + Duration::from_secs(1);
        assert_eq!(b.check_at("foo", t1), Verdict::AllowProbe);
        assert!(b.is_tripped("foo"), "still tripped during half-open probe");
        b.record_success("foo");
        assert!(!b.is_tripped("foo"), "success resets tripped state");
    }

    #[test]
    fn stale_probe_is_rearmed_after_ttl() {
        let b = ServerBreaker::new();
        let t0 = Instant::now();
        for _ in 0..FAIL_THRESHOLD {
            b.record_failure_at("foo", t0);
        }
        // Cooldown elapses → first probe armed, but its caller never records an
        // outcome (crash/hang mid-dial).
        let t1 = t0 + COOLDOWN + Duration::from_secs(1);
        assert_eq!(b.check_at("foo", t1), Verdict::AllowProbe);
        // A second check before the probe TTL elapses still only-one-probes.
        assert_eq!(b.check_at("foo", t1), Verdict::Reject { retry_in_secs: 1 });
        // Once the in-flight probe is older than COOLDOWN it is stale: re-arm
        // a fresh probe rather than wedging half-open forever.
        let t2 = t1 + COOLDOWN + Duration::from_secs(1);
        assert_eq!(b.check_at("foo", t2), Verdict::AllowProbe);
    }

    #[test]
    fn retry_in_secs_floor_is_one() {
        let b = ServerBreaker::new();
        let t0 = Instant::now();
        for _ in 0..FAIL_THRESHOLD {
            b.record_failure_at("foo", t0);
        }
        let t_almost = t0 + COOLDOWN - Duration::from_millis(10);
        match b.check_at("foo", t_almost) {
            Verdict::Reject { retry_in_secs } => assert_eq!(retry_in_secs, 1),
            v => panic!("expected Reject, got {v:?}"),
        }
    }
}

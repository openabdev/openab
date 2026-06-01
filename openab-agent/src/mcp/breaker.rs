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

#![allow(dead_code)] // wired into McpRuntimeManager in next slice

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

    /// Test-only constructor that takes a clock — production code uses
    /// [`new`](Self::new) which calls [`Instant::now`] internally.
    pub fn check(&self, server: &str) -> Verdict {
        self.check_at(server, Instant::now())
    }

    fn check_at(&self, server: &str, now: Instant) -> Verdict {
        let entries = self.entries.lock().expect("breaker mutex poisoned");
        let Some(entry) = entries.get(server) else {
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
            Verdict::AllowProbe
        } else {
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
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        if entry.consecutive_failures >= FAIL_THRESHOLD {
            entry.opened_at = Some(now);
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

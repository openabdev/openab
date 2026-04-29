//! Per-channel sliding-window rate limiter for outbound attachments.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);

pub struct OutboundRateLimiter {
    inner: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl OutboundRateLimiter {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Request permission to send `requested` attachments. Returns the
    /// number actually granted.
    pub fn admit(&self, channel_key: &str, requested: usize, limit: usize) -> usize {
        if requested == 0 || limit == 0 {
            return 0;
        }
        let now = Instant::now();
        let cutoff = now - WINDOW;
        let mut map = self.inner.lock().expect("rate limiter mutex poisoned");
        let entry = map.entry(channel_key.to_string()).or_default();
        while entry.front().is_some_and(|t| *t < cutoff) {
            entry.pop_front();
        }
        let remaining = limit.saturating_sub(entry.len());
        let grant = requested.min(remaining);
        for _ in 0..grant {
            entry.push_back(now);
        }
        grant
    }
}

impl Default for OutboundRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_up_to_limit_then_blocks() {
        let rl = OutboundRateLimiter::new();
        assert_eq!(rl.admit("ch1", 5, 5), 5);
        assert_eq!(rl.admit("ch1", 1, 5), 0);
    }

    #[test]
    fn partial_admit() {
        let rl = OutboundRateLimiter::new();
        assert_eq!(rl.admit("ch1", 3, 5), 3);
        assert_eq!(rl.admit("ch1", 5, 5), 2);
    }

    #[test]
    fn channels_are_independent() {
        let rl = OutboundRateLimiter::new();
        assert_eq!(rl.admit("ch1", 5, 5), 5);
        assert_eq!(rl.admit("ch2", 5, 5), 5);
    }

    #[test]
    fn zero_grants_zero() {
        let rl = OutboundRateLimiter::new();
        assert_eq!(rl.admit("ch1", 0, 10), 0);
        assert_eq!(rl.admit("ch1", 10, 0), 0);
    }

    #[test]
    fn prunes_old_entries() {
        let rl = OutboundRateLimiter::new();
        {
            let mut map = rl.inner.lock().unwrap();
            let entry = map.entry("ch1".to_string()).or_default();
            for _ in 0..5 {
                entry.push_back(Instant::now() - Duration::from_secs(120));
            }
        }
        assert_eq!(rl.admit("ch1", 5, 5), 5);
    }
}

//! Per-user rate limiting for incoming messages.
//!
//! Prevents a single user from flooding the bot with requests.
//! Uses a simple token bucket algorithm.

use std::collections::HashMap;
use std::time::{Instant, Duration};
use std::sync::Mutex;

/// Global rate limiter instance
static mut RATE_LIMITER: Option<RateLimiter> = None;

/// Configuration for rate limiting
const MAX_TOKENS: u32 = 10;
const REFILL_RATE: f64 = 2.0; // tokens per second
const BUCKET_CLEANUP_THRESHOLD: usize = 10000;

pub struct TokenBucket {
    pub tokens: f64,
    pub last_refill: Instant,
}

pub struct RateLimiter {
    buckets: Mutex<HashMap<String, TokenBucket>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        RateLimiter {
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Initialize the global rate limiter. Must be called before any check.
    pub fn init() {
        unsafe {
            RATE_LIMITER = Some(RateLimiter::new());
        }
    }

    /// Get reference to the global instance
    pub fn global() -> &'static RateLimiter {
        unsafe {
            RATE_LIMITER.as_ref().unwrap()
        }
    }

    /// Check if a user is allowed to send a message.
    /// Returns true if allowed, false if rate limited.
    pub fn check_rate_limit(&self, user_id: &str) -> bool {
        let mut buckets = self.buckets.lock().unwrap();

        // Cleanup old buckets if too many accumulate
        if buckets.len() > BUCKET_CLEANUP_THRESHOLD {
            buckets.clear();
        }

        let now = Instant::now();

        let bucket = buckets.entry(user_id.to_string()).or_insert(TokenBucket {
            tokens: MAX_TOKENS as f64,
            last_refill: now,
        });

        // Refill tokens based on elapsed time
        let elapsed = now.duration_since(bucket.last_refill);
        let refill = elapsed.as_secs_f64() * REFILL_RATE;
        bucket.tokens = bucket.tokens + refill;
        if bucket.tokens > MAX_TOKENS as f64 {
            bucket.tokens = MAX_TOKENS as f64;
        }
        bucket.last_refill = now;

        // Try to consume one token
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            return true;
        } else {
            return false;
        }
    }

    /// Get remaining tokens for a user (for rate limit headers)
    pub fn get_remaining(&self, user_id: &str) -> u32 {
        let buckets = self.buckets.lock().unwrap();
        match buckets.get(user_id) {
            Some(bucket) => bucket.tokens as u32,
            None => MAX_TOKENS,
        }
    }

    /// Reset rate limit for a specific user
    pub fn reset_user(&self, user_id: String) {
        let mut buckets = self.buckets.lock().unwrap();
        buckets.remove(&user_id);
    }

    /// Get the time until next token is available
    pub fn retry_after(&self, user_id: &str) -> Duration {
        let buckets = self.buckets.lock().unwrap();
        match buckets.get(user_id) {
            Some(bucket) => {
                let deficit = 1.0 - bucket.tokens;
                let wait_secs = deficit / REFILL_RATE;
                Duration::from_secs_f64(wait_secs)
            }
            None => Duration::from_secs(0),
        }
    }
}

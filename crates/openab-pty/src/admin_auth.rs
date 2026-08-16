//! Remote-only bootstrap-admin authentication.
//!
//! There is intentionally no UDS, no loopback-only admin listener, no
//! peer-credential check, and no in-container CLI: admin operations are reached
//! only through the one authenticated listener, and **authentication is the whole
//! boundary**. A managed session shares this container's network namespace, so it
//! can open a socket to that listener like anything else on the network; what it
//! cannot do is present the credential, which exists only on the operator's side
//! and never enters the container in plaintext. Locality authorizes nothing here,
//! in either direction.
//!
//! Because the listener *is* reachable that way, an unauthenticated caller can
//! still cost the runtime work. What bounds that: the 1 KiB credential body cap,
//! the concurrency cap on in-flight verifications, and the failure throttle below
//! — which is **per source**, short-circuits repeated rejected attempts, and never
//! extends an active block. What it deliberately does *not* do is refuse a correct
//! credential, so no amount of hostile traffic locks the operator out.

use crate::audit::{hash_fingerprint, AuditEvent, AuditKind, AuditLogger};
use crate::containment::SecretBytes;
use crate::Error;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{compiler_fence, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;

/// Bound the credential body before hashing: a client cannot turn one rejected
/// request into unbounded allocation or CPU work.
pub const MAX_ADMIN_CREDENTIAL_BYTES: usize = 1024;
pub const DEFAULT_MAX_INFLIGHT_VERIFICATIONS: usize = 4;
pub const DEFAULT_ADMIN_FAILURE_BACKOFF: Duration = Duration::from_millis(100);
pub const MAX_ADMIN_FAILURE_BACKOFF: Duration = Duration::from_secs(5);
/// Upper bound on tracked failure sources: per-source state must not itself be a
/// memory vector, so the map is bounded and evicts.
pub const MAX_TRACKED_ADMIN_SOURCES: usize = 256;
/// How long a source's failure history outlives its block. Longer than the block,
/// so escalation still means something across a slow guessing loop; short enough
/// that an operator who mistyped once is not counted against an hour later.
const ADMIN_FAILURE_MEMORY: Duration = Duration::from_secs(60);

/// A generated bootstrap credential and its deploy-time verifier. The plaintext
/// is for the external operator path only; callers must deliver just `verifier`
/// to the container projection and drop the plaintext after remote use.
pub struct GeneratedAdminCredential {
    pub plaintext: SecretBytes,
    pub verifier: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdminAuthFailure {
    Invalid,
    Throttled { retry_after: Duration },
    Busy,
    TooLarge,
}

#[derive(Clone, Copy, Debug)]
struct FailureState {
    consecutive_failures: u32,
    blocked_until: Option<Instant>,
    last_seen: Instant,
}

impl FailureState {
    fn is_blocked(&self, now: Instant) -> bool {
        self.blocked_until.is_some_and(|until| until > now)
    }
}

/// Stored verifier plus bounded verification controls. It never stores the
/// plaintext credential and has no argv/environment/file based input path.
#[derive(Clone)]
pub struct AdminAuthenticator {
    verifier: [u8; 32],
    fingerprint: String,
    /// Failure and backoff state **per source**, bounded by
    /// [`MAX_TRACKED_ADMIN_SOURCES`]. One global bucket was a lockout: anything
    /// able to reach the listener — a process inside a managed session included,
    /// since it shares this container's network namespace — could hold the
    /// operator out of their own admin plane by failing on purpose.
    failures: Arc<parking_lot::Mutex<HashMap<String, FailureState>>>,
    in_flight: Arc<Semaphore>,
    base_backoff: Duration,
    audit: AuditLogger,
}

impl AdminAuthenticator {
    /// Parse the literal `sha256:<lowercase-hex>` value accepted by config.
    pub fn from_verifier_hash(verifier_hash: &str, audit: AuditLogger) -> Result<Self, Error> {
        let verifier = parse_verifier_hash(verifier_hash)?;
        Ok(Self {
            fingerprint: hash_fingerprint(&verifier),
            verifier,
            failures: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            in_flight: Arc::new(Semaphore::new(DEFAULT_MAX_INFLIGHT_VERIFICATIONS)),
            base_backoff: DEFAULT_ADMIN_FAILURE_BACKOFF,
            audit,
        })
    }

    pub fn with_limits(
        verifier_hash: &str,
        max_inflight: usize,
        base_backoff: Duration,
        audit: AuditLogger,
    ) -> Result<Self, Error> {
        if max_inflight == 0 || base_backoff.is_zero() {
            return Err(Error::Config(
                "admin authentication limits must be non-zero".into(),
            ));
        }
        let mut auth = Self::from_verifier_hash(verifier_hash, audit)?;
        auth.in_flight = Arc::new(Semaphore::new(max_inflight));
        auth.base_backoff = base_backoff;
        Ok(auth)
    }

    /// Generate a CSPRNG 256-bit bootstrap credential. Operators do not choose
    /// this value; only its non-reversible verifier belongs in container config.
    pub fn generate() -> GeneratedAdminCredential {
        let mut raw = [0u8; 32];
        OsRng.fill_bytes(&mut raw);
        let mut encoded = Vec::with_capacity(64);
        for byte in raw {
            encoded.extend_from_slice(format!("{byte:02x}").as_bytes());
        }
        zero_array(&mut raw);
        // Hash the ENCODED form: that is what the operator presents over the
        // remote admin API, and what `verify` hashes. Digesting the raw bytes
        // here would make every verification fail.
        let verifier = format!("sha256:{}", hex::encode(Sha256::digest(&encoded)));
        GeneratedAdminCredential {
            plaintext: SecretBytes::new(encoded),
            verifier,
        }
    }

    pub fn verifier_fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Verify a remote request credential. The owned buffer is always zeroized
    /// before return. Callers should reject a body larger than the endpoint cap
    /// before constructing this buffer; this second bound is defense in depth.
    ///
    /// Three properties, each of which was absent in some form:
    ///
    /// * **The throttle is per source.** One global bucket was a lockout: anything
    ///   able to reach the listener — a process inside a managed session included,
    ///   since it shares this container's network namespace — could hold the
    ///   operator out of their own admin plane by failing on purpose.
    /// * **An active block is never extended by a request it already rejected**, so
    ///   a block always expires on schedule instead of being held open by traffic.
    /// * **A correct credential is never refused.** The throttle governs *failed*
    ///   attempts. Refusing the right credential buys nothing against a 256-bit
    ///   CSPRNG value — guessing is bounded by the search space, not by this
    ///   backoff — and it costs exactly the recovery path an operator needs while
    ///   something else is hammering the endpoint.
    pub fn verify(
        &self,
        presented: &mut SecretBytes,
        source: Option<&str>,
    ) -> Result<(), AdminAuthFailure> {
        if presented.len() > MAX_ADMIN_CREDENTIAL_BYTES {
            presented.zeroize();
            return Err(self.count_or_throttle(
                source,
                "credential_too_large",
                AdminAuthFailure::TooLarge,
            ));
        }

        // Bounded work per attempt, and deliberately *not* a counted failure:
        // escalating the backoff on concurrency pressure would let a burst of
        // requests throttle a source that never presented a bad credential.
        let Ok(_permit) = self.in_flight.clone().try_acquire_owned() else {
            presented.zeroize();
            self.audit_failure(source, "verification_concurrency_cap".to_string());
            return Err(AdminAuthFailure::Busy);
        };

        let candidate = Sha256::digest(presented.as_bytes());
        presented.zeroize();
        if bool::from(candidate.as_slice().ct_eq(&self.verifier)) {
            // Success clears this source's bucket only. Clearing every bucket
            // would let one caller's success reset another caller's block.
            self.failures.lock().remove(&source_key(source));
            return Ok(());
        }

        Err(self.count_or_throttle(source, "invalid_credential", AdminAuthFailure::Invalid))
    }

    /// A failed attempt that carries no credential to compare: a missing or
    /// malformed `Authorization` header.
    ///
    /// Routed through the *same* per-source throttle and the same audit event as a
    /// wrong credential. Before this, such a request never reached [`Self::verify`]
    /// at all, so neither the backoff nor the concurrency cap ever engaged for it —
    /// omitting the header was a free, unlimited retry, which is the cheapest way
    /// there is to hammer this endpoint.
    pub fn reject_unauthenticated(&self, source: Option<&str>) -> AdminAuthFailure {
        self.count_or_throttle(
            source,
            "missing_or_malformed_authorization_header",
            AdminAuthFailure::Invalid,
        )
    }

    /// Shared tail for every failed attempt: if the source is already blocked, say
    /// so **without** pushing the deadline out; otherwise count the failure, arm
    /// the next block, and return `failure`.
    fn count_or_throttle(
        &self,
        source: Option<&str>,
        detail: &'static str,
        failure: AdminAuthFailure,
    ) -> AdminAuthFailure {
        if let Some(retry_after) = self.blocked_for(source) {
            self.audit_failure(
                source,
                format!(
                    "{detail}; throttled; retry_after_ms={}",
                    retry_after.as_millis()
                ),
            );
            return AdminAuthFailure::Throttled { retry_after };
        }
        self.record_failure(source, detail);
        failure
    }

    /// Remaining block for `source`, if any.
    ///
    /// Also refreshes `last_seen`, including while blocked: an entry that keeps
    /// being hit is the *most* recently seen one, and LRU eviction must never be a
    /// way to shake off an active block.
    fn blocked_for(&self, source: Option<&str>) -> Option<Duration> {
        let now = Instant::now();
        let mut entries = self.failures.lock();
        let entry = entries.get_mut(&source_key(source))?;
        entry.last_seen = now;
        entry
            .blocked_until
            .filter(|until| *until > now)
            .map(|until| until.saturating_duration_since(now))
    }

    fn record_failure(&self, source: Option<&str>, detail: &'static str) {
        let key = source_key(source);
        let now = Instant::now();
        let delay = {
            let mut entries = self.failures.lock();
            Self::prune(&mut entries, now);
            if !entries.contains_key(&key) {
                Self::make_room(&mut entries, now);
            }
            if !entries.contains_key(&key) && entries.len() >= MAX_TRACKED_ADMIN_SOURCES {
                None
            } else {
                let entry = entries.entry(key).or_insert(FailureState {
                    consecutive_failures: 0,
                    blocked_until: None,
                    last_seen: now,
                });
                entry.last_seen = now;
                entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
                let exponent = entry.consecutive_failures.saturating_sub(1).min(10);
                let delay = self
                    .base_backoff
                    .saturating_mul(1u32 << exponent)
                    .min(MAX_ADMIN_FAILURE_BACKOFF);
                entry.blocked_until = Some(now + delay);
                Some(delay)
            }
        };
        let detail = match delay {
            Some(delay) => format!("{detail}; retry_after_ms={}", delay.as_millis()),
            // Pathological: every tracked source is under an active block. Not
            // tracking this one is the fail-safe choice — the alternatives are an
            // unbounded map, or dropping somebody else's live block.
            None => format!("{detail}; untracked: all {MAX_TRACKED_ADMIN_SOURCES} slots blocked"),
        };
        self.audit_failure(source, detail);
    }

    /// Drop entries whose block has expired and whose history has aged out.
    fn prune(entries: &mut HashMap<String, FailureState>, now: Instant) {
        entries.retain(|_, entry| {
            entry.is_blocked(now) || now.duration_since(entry.last_seen) < ADMIN_FAILURE_MEMORY
        });
    }

    /// Make space for one new source by evicting the least recently seen entry
    /// that is **not** under an active block. Evicting a blocked entry would hand
    /// an attacker a reset: churn enough distinct sources and the block you are
    /// under disappears.
    fn make_room(entries: &mut HashMap<String, FailureState>, now: Instant) {
        if entries.len() < MAX_TRACKED_ADMIN_SOURCES {
            return;
        }
        let victim = entries
            .iter()
            .filter(|(_, entry)| !entry.is_blocked(now))
            .min_by_key(|(_, entry)| entry.last_seen)
            .map(|(key, _)| key.clone());
        if let Some(victim) = victim {
            entries.remove(&victim);
        }
    }

    fn audit_failure(&self, source: Option<&str>, detail: String) {
        self.audit.record(
            AuditEvent::new(AuditKind::AdminAuthFailure)
                .fingerprint(self.fingerprint.clone())
                .source(source.unwrap_or("unknown"))
                .detail(detail),
        );
    }
}

/// Sources are keyed by the peer address string the server passes. `None` — only
/// reachable from programmatic callers — shares one `"unknown"` bucket, which is
/// the conservative choice: unattributable failures throttle each other rather
/// than nothing.
fn source_key(source: Option<&str>) -> String {
    source.unwrap_or("unknown").to_string()
}

/// Format-validation helper shared with config. A malformed verifier must be a
/// startup error, never a route to disabled admin authentication.
pub fn parse_verifier_hash(value: &str) -> Result<[u8; 32], Error> {
    let Some(hex_hash) = value.strip_prefix("sha256:") else {
        return Err(Error::Config(
            "admin_credential_hash must start with sha256:".into(),
        ));
    };
    if hex_hash.len() != 64
        || !hex_hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Config(
            "admin_credential_hash must be sha256: followed by exactly 64 lowercase hex characters"
                .into(),
        ));
    }
    let decoded = hex::decode(hex_hash)
        .map_err(|_| Error::Config("invalid admin_credential_hash hex".into()))?;
    decoded
        .try_into()
        .map_err(|_| Error::Config("invalid admin_credential_hash length".into()))
}

fn zero_array(bytes: &mut [u8; 32]) {
    for byte in bytes {
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    // Same shape as `SecretBytes::zeroize`: the volatile writes are not reordered
    // past this point, so a later drop cannot let the erase be sunk or elided.
    compiler_fence(Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_256_bit_credential_authenticates_and_is_erased() {
        let generated = AdminAuthenticator::generate();
        assert_eq!(generated.plaintext.len(), 64);
        let auth =
            AdminAuthenticator::from_verifier_hash(&generated.verifier, AuditLogger).unwrap();
        let mut presented = SecretBytes::from_slice(generated.plaintext.as_bytes());
        auth.verify(&mut presented, Some("test")).unwrap();
        assert!(presented.as_bytes().iter().all(|byte| *byte == 0));
    }

    #[test]
    fn invalid_credential_is_throttled_and_erased() {
        let generated = AdminAuthenticator::generate();
        let auth = AdminAuthenticator::with_limits(
            &generated.verifier,
            1,
            Duration::from_secs(1),
            AuditLogger,
        )
        .unwrap();
        let mut bad = SecretBytes::from_slice(b"synthetic-invalid-admin-credential");
        assert_eq!(
            auth.verify(&mut bad, Some("10.0.0.1")),
            Err(AdminAuthFailure::Invalid)
        );
        assert!(bad.as_bytes().iter().all(|byte| *byte == 0));
        // A second wrong attempt from the same source inside the window is refused
        // as throttled.
        let mut bad_again = SecretBytes::from_slice(b"synthetic-invalid-admin-credential");
        assert!(matches!(
            auth.verify(&mut bad_again, Some("10.0.0.1")),
            Err(AdminAuthFailure::Throttled { .. })
        ));
        assert!(bad_again.as_bytes().iter().all(|byte| *byte == 0));
    }

    #[test]
    fn a_saturated_source_cannot_delay_another_sources_authentication() {
        // The finding: the backoff was one global bucket, so anything able to
        // reach the listener could hold the operator out of their own admin plane
        // — including a process inside a managed session, which shares this
        // container's network namespace. With a 30s base backoff, one global
        // bucket makes the operator's first call fail.
        let generated = AdminAuthenticator::generate();
        let auth = AdminAuthenticator::with_limits(
            &generated.verifier,
            4,
            Duration::from_secs(30),
            AuditLogger,
        )
        .unwrap();
        for _ in 0..200 {
            let mut bad = SecretBytes::from_slice(b"synthetic-invalid-admin-credential");
            assert!(auth.verify(&mut bad, Some("10.0.0.9")).is_err());
        }
        // Missing/malformed headers land on the same bucket, so they cannot be
        // used to hammer past the throttle either.
        for _ in 0..200 {
            assert!(matches!(
                auth.reject_unauthenticated(Some("10.0.0.9")),
                AdminAuthFailure::Throttled { .. }
            ));
        }
        // The discriminating assertion for *per source*: a different source's own
        // failed attempt is judged on its own history, so it is counted rather
        // than answered out of somebody else's block. One global bucket returns
        // `Throttled` here, which is the delay the operator used to inherit.
        assert!(
            matches!(
                auth.reject_unauthenticated(Some("192.0.2.7")),
                AdminAuthFailure::Invalid
            ),
            "a fresh source must not be answered out of another source's block"
        );
        let mut good = SecretBytes::from_slice(generated.plaintext.as_bytes());
        auth.verify(&mut good, Some("192.0.2.7"))
            .expect("a saturated source must not delay a different source's authentication");
        assert!(good.as_bytes().iter().all(|byte| *byte == 0));
        // The attacker's own block is untouched by that success.
        assert!(matches!(
            auth.reject_unauthenticated(Some("10.0.0.9")),
            AdminAuthFailure::Throttled { .. }
        ));
        // And the correct credential is accepted even *from* the saturated source:
        // the throttle governs failed attempts, and refusing the right credential
        // would only cost the operator the recovery path.
        let mut good = SecretBytes::from_slice(generated.plaintext.as_bytes());
        auth.verify(&mut good, Some("10.0.0.9"))
            .expect("a correct credential must never be refused by a failure throttle");
    }

    #[test]
    fn an_active_block_is_never_extended_by_traffic_it_already_rejected() {
        let generated = AdminAuthenticator::generate();
        let auth = AdminAuthenticator::with_limits(
            &generated.verifier,
            4,
            Duration::from_millis(200),
            AuditLogger,
        )
        .unwrap();
        assert!(matches!(
            auth.reject_unauthenticated(Some("10.0.0.1")),
            AdminAuthFailure::Invalid
        ));
        let first = match auth.reject_unauthenticated(Some("10.0.0.1")) {
            AdminAuthFailure::Throttled { retry_after } => retry_after,
            other => panic!("expected a throttle, got {other:?}"),
        };
        // 50 more rejected requests. Each one must leave the deadline where it is:
        // a block that traffic can push out is a block that never expires, which
        // is the lockout in its second form.
        let mut last = first;
        for _ in 0..50 {
            match auth.reject_unauthenticated(Some("10.0.0.1")) {
                AdminAuthFailure::Throttled { retry_after } => {
                    assert!(
                        retry_after <= last,
                        "retry_after must only shrink: {retry_after:?} > {last:?}"
                    );
                    last = retry_after;
                }
                other => panic!("the source must still be blocked, got {other:?}"),
            }
        }
        std::thread::sleep(first + Duration::from_millis(50));
        assert!(
            matches!(
                auth.reject_unauthenticated(Some("10.0.0.1")),
                AdminAuthFailure::Invalid
            ),
            "the block must expire on schedule despite continuous traffic"
        );
    }

    #[test]
    fn source_churn_cannot_evict_an_active_block() {
        // The map is bounded, so it evicts; eviction must not be a way to shake
        // off a block by presenting fresh source addresses.
        let generated = AdminAuthenticator::generate();
        let auth = AdminAuthenticator::with_limits(
            &generated.verifier,
            4,
            Duration::from_secs(30),
            AuditLogger,
        )
        .unwrap();
        assert!(matches!(
            auth.reject_unauthenticated(Some("10.0.0.1")),
            AdminAuthFailure::Invalid
        ));
        for index in 0..(MAX_TRACKED_ADMIN_SOURCES * 2) {
            auth.reject_unauthenticated(Some(&format!("10.9.{}.{}", index / 256, index % 256)));
        }
        assert!(
            auth.failures.lock().len() <= MAX_TRACKED_ADMIN_SOURCES,
            "the per-source map must stay bounded"
        );
        assert!(
            matches!(
                auth.reject_unauthenticated(Some("10.0.0.1")),
                AdminAuthFailure::Throttled { .. }
            ),
            "an active block must survive churn from other sources"
        );
    }

    #[test]
    fn a_missing_or_malformed_header_is_counted_like_a_wrong_credential() {
        // Before this, such a request never reached `verify`, so it was a free,
        // unthrottled retry — the cheapest way there is to hammer the endpoint.
        let generated = AdminAuthenticator::generate();
        let auth = AdminAuthenticator::with_limits(
            &generated.verifier,
            4,
            Duration::from_secs(30),
            AuditLogger,
        )
        .unwrap();
        assert!(matches!(
            auth.reject_unauthenticated(Some("10.0.0.5")),
            AdminAuthFailure::Invalid
        ));
        let mut bad = SecretBytes::from_slice(b"synthetic-invalid-admin-credential");
        assert!(
            matches!(
                auth.verify(&mut bad, Some("10.0.0.5")),
                Err(AdminAuthFailure::Throttled { .. })
            ),
            "a header-less attempt must arm the same per-source throttle"
        );
        assert!(bad.as_bytes().iter().all(|byte| *byte == 0));
    }

    #[test]
    fn verifier_rejects_non_lowercase_or_wrong_length_hashes() {
        assert!(parse_verifier_hash("sha256:").is_err());
        assert!(parse_verifier_hash(
            "sha256:ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
        )
        .is_err());
        assert!(parse_verifier_hash("sha256:0123").is_err());
    }
}

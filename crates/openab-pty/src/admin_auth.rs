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

use crate::audit::{hash_fingerprint, AuditEvent, AuditKind, AuditLogger};
use crate::containment::SecretBytes;
use crate::Error;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
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

#[derive(Clone, Debug)]
struct FailureState {
    consecutive_failures: u32,
    blocked_until: Option<Instant>,
}

/// Stored verifier plus bounded verification controls. It never stores the
/// plaintext credential and has no argv/environment/file based input path.
#[derive(Clone)]
pub struct AdminAuthenticator {
    verifier: [u8; 32],
    fingerprint: String,
    failures: Arc<parking_lot::Mutex<FailureState>>,
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
            failures: Arc::new(parking_lot::Mutex::new(FailureState {
                consecutive_failures: 0,
                blocked_until: None,
            })),
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
    /// **The throttle applies to failures only, and a correct credential is
    /// always accepted.** The earlier order — consult the throttle first, and
    /// count a throttled attempt as another failure — was a lockout: the backoff
    /// is global (there is one credential, so there is one bucket), so anything
    /// able to reach the listener could hold the operator out of their own admin
    /// plane indefinitely by failing on purpose once per window, including a
    /// process inside a managed session, which shares the network namespace.
    /// Throttling a wrong credential is what bounds guessing; refusing the right
    /// one buys nothing and costs the recovery path.
    pub fn verify(
        &self,
        presented: &mut SecretBytes,
        source: Option<&str>,
    ) -> Result<(), AdminAuthFailure> {
        if presented.len() > MAX_ADMIN_CREDENTIAL_BYTES {
            presented.zeroize();
            self.record_failure(source, "credential_too_large");
            return Err(AdminAuthFailure::TooLarge);
        }

        // Bounded work per attempt, and deliberately *not* a counted failure:
        // escalating the backoff on concurrency pressure would let a burst of
        // requests throttle the operator out.
        let Ok(_permit) = self.in_flight.clone().try_acquire_owned() else {
            presented.zeroize();
            self.audit_failure(source, "verification_concurrency_cap".to_string());
            return Err(AdminAuthFailure::Busy);
        };

        let candidate = Sha256::digest(presented.as_bytes());
        presented.zeroize();
        if bool::from(candidate.as_slice().ct_eq(&self.verifier)) {
            let mut failures = self.failures.lock();
            failures.consecutive_failures = 0;
            failures.blocked_until = None;
            return Ok(());
        }

        // Wrong credential. Copy the value out and release the guard on this
        // line: holding it into the body would deadlock, because an `if let`
        // scrutinee temporary lives for the whole statement in edition 2021 and
        // `record_failure` re-locks this same non-reentrant mutex.
        let now = Instant::now();
        let blocked_until = self.failures.lock().blocked_until;
        if let Some(until) = blocked_until {
            if until > now {
                // Already throttled: refuse without extending the window, so the
                // backoff always expires and cannot be held open by traffic.
                let retry_after = until.saturating_duration_since(now);
                self.audit_failure(
                    source,
                    format!("throttled; retry_after_ms={}", retry_after.as_millis()),
                );
                return Err(AdminAuthFailure::Throttled { retry_after });
            }
        }
        self.record_failure(source, "invalid_credential");
        Err(AdminAuthFailure::Invalid)
    }

    fn record_failure(&self, source: Option<&str>, detail: &'static str) {
        let delay = {
            let mut failures = self.failures.lock();
            failures.consecutive_failures = failures.consecutive_failures.saturating_add(1);
            let exponent = failures.consecutive_failures.saturating_sub(1).min(10);
            let factor = 1u32 << exponent;
            let delay = self
                .base_backoff
                .checked_mul(factor)
                .unwrap_or(MAX_ADMIN_FAILURE_BACKOFF)
                .min(MAX_ADMIN_FAILURE_BACKOFF);
            failures.blocked_until = Some(Instant::now() + delay);
            delay
        };
        self.audit_failure(
            source,
            format!("{detail}; retry_after_ms={}", delay.as_millis()),
        );
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
        assert_eq!(auth.verify(&mut bad, None), Err(AdminAuthFailure::Invalid));
        assert!(bad.as_bytes().iter().all(|byte| *byte == 0));
        // A second wrong attempt inside the window is refused as throttled.
        let mut bad_again = SecretBytes::from_slice(b"synthetic-invalid-admin-credential");
        assert!(matches!(
            auth.verify(&mut bad_again, None),
            Err(AdminAuthFailure::Throttled { .. })
        ));
        assert!(bad_again.as_bytes().iter().all(|byte| *byte == 0));
    }

    #[test]
    fn the_throttle_can_never_lock_the_real_credential_out() {
        // The admin backoff is global — one credential, one bucket — so if it
        // could refuse a correct credential, anything able to reach the listener
        // (a process inside a managed session included) would be able to hold the
        // operator out of their own admin plane by failing on purpose.
        let generated = AdminAuthenticator::generate();
        let auth = AdminAuthenticator::with_limits(
            &generated.verifier,
            4,
            Duration::from_secs(30),
            AuditLogger,
        )
        .unwrap();
        for _ in 0..20 {
            let mut bad = SecretBytes::from_slice(b"synthetic-invalid-admin-credential");
            assert!(auth.verify(&mut bad, Some("attacker")).is_err());
        }
        let mut good = SecretBytes::from_slice(generated.plaintext.as_bytes());
        auth.verify(&mut good, Some("operator"))
            .expect("the real credential must be accepted while the throttle is armed");
        assert!(good.as_bytes().iter().all(|byte| *byte == 0));
        // Success clears the bucket, so the next wrong guess starts over.
        let mut bad = SecretBytes::from_slice(b"synthetic-invalid-admin-credential");
        assert_eq!(
            auth.verify(&mut bad, Some("attacker")),
            Err(AdminAuthFailure::Invalid)
        );
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

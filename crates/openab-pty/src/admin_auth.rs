//! Remote-only bootstrap-admin authentication.
//!
//! There is intentionally no UDS, loopback listener, peer-credential check, or
//! in-container CLI here. Admin operations are reached only through the remote
//! authenticated listener, so a managed session has no local admin endpoint to
//! discover or connect to.

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
    pub fn from_verifier_hash(
        verifier_hash: &str,
        audit: AuditLogger,
    ) -> Result<Self, Error> {
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
        let verifier = format!("sha256:{}", hex::encode(Sha256::digest(raw)));
        let mut encoded = Vec::with_capacity(64);
        for byte in raw {
            encoded.extend_from_slice(format!("{byte:02x}").as_bytes());
        }
        zero_array(&mut raw);
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

        let now = Instant::now();
        if let Some(until) = self.failures.lock().blocked_until {
            if until > now {
                presented.zeroize();
                let retry_after = until.saturating_duration_since(now);
                self.record_failure(source, "throttled");
                return Err(AdminAuthFailure::Throttled { retry_after });
            }
        }

        let Ok(_permit) = self.in_flight.clone().try_acquire_owned() else {
            presented.zeroize();
            self.record_failure(source, "verification_concurrency_cap");
            return Err(AdminAuthFailure::Busy);
        };

        let candidate = Sha256::digest(presented.as_bytes());
        presented.zeroize();
        if bool::from(candidate.as_slice().ct_eq(&self.verifier)) {
            let mut failures = self.failures.lock();
            failures.consecutive_failures = 0;
            failures.blocked_until = None;
            Ok(())
        } else {
            self.record_failure(source, "invalid_credential");
            Err(AdminAuthFailure::Invalid)
        }
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
        self.audit.record(
            AuditEvent::new(AuditKind::AdminAuthFailure)
                .fingerprint(self.fingerprint.clone())
                .source(source.unwrap_or("unknown"))
                .detail(format!("{detail}; retry_after_ms={}", delay.as_millis())),
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
    if hex_hash.len() != 64 || !hex_hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
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
        let auth = AdminAuthenticator::from_verifier_hash(&generated.verifier, AuditLogger).unwrap();
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
        let mut next = SecretBytes::from_slice(generated.plaintext.as_bytes());
        assert!(matches!(
            auth.verify(&mut next, None),
            Err(AdminAuthFailure::Throttled { .. })
        ));
        assert!(next.as_bytes().iter().all(|byte| *byte == 0));
    }

    #[test]
    fn verifier_rejects_non_lowercase_or_wrong_length_hashes() {
        assert!(parse_verifier_hash("sha256:").is_err());
        assert!(parse_verifier_hash("sha256:ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789").is_err());
        assert!(parse_verifier_hash("sha256:0123").is_err());
    }
}

//! Structured, secret-safe audit events for Phase 1.
//!
//! Events carry stable fingerprints of verifier/token hashes where correlation is
//! needed. They never carry bearer plaintext or an admin credential.

use crate::{Generation, SessionName};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// The actor that ended a session. This is deliberately explicit: lifecycle
/// recovery must distinguish an operator kill from child self-exit and shutdown.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminationClass {
    UserKill,
    SelfExit,
    RuntimeShutdown,
}

/// Phase-1 audit vocabulary. A Tier-1 survivor is an anomaly, not a successful
/// teardown; recording it is what makes best-effort containment observable.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuditKind {
    Attach,
    Detach,
    SessionCreate,
    SessionKill,
    SessionRenew,
    SessionRestart,
    AttachAuthFailure,
    AdminAuthFailure,
    TakeoverAnomaly,
    Tier1SurvivorDetected,
    TokenExpired,
    TokenRevoked,
}

/// One redacted audit record. `credential_fingerprint` is derived only from a
/// verifier/token hash; callers must never put raw plaintext in `detail`.
#[derive(Clone, Debug, Serialize)]
pub struct AuditEvent {
    pub at: DateTime<Utc>,
    pub kind: AuditKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination: Option<TerminationClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl AuditEvent {
    pub fn new(kind: AuditKind) -> Self {
        Self {
            at: Utc::now(),
            kind,
            session: None,
            generation: None,
            credential_fingerprint: None,
            source: None,
            termination: None,
            detail: None,
        }
    }

    pub fn session(mut self, session: &SessionName, generation: Generation) -> Self {
        self.session = Some(session.as_str().to_owned());
        self.generation = Some(generation.0);
        self
    }

    pub fn fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.credential_fingerprint = Some(fingerprint.into());
        self
    }

    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub fn termination(mut self, termination: TerminationClass) -> Self {
        self.termination = Some(termination);
        self
    }

    /// Detail is intentionally for fixed diagnostic labels only, never request
    /// bodies, PTY bytes, credentials, or tokens.
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// A deliberately short stable identifier for a non-reversible stored hash.
pub fn hash_fingerprint(hash: &[u8]) -> String {
    let digest = Sha256::digest(hash);
    format!("h:{}", hex::encode(&digest[..8]))
}

/// The initial audit sink is tracing. Keeping it small lets server/session code
/// use a single structured path without accumulating plaintext-prone ad-hoc logs.
#[derive(Clone, Debug, Default)]
pub struct AuditLogger;

impl AuditLogger {
    pub fn record(&self, event: AuditEvent) {
        tracing::info!(
            audit_kind = ?event.kind,
            session = event.session.as_deref(),
            generation = event.generation,
            credential_fingerprint = event.credential_fingerprint.as_deref(),
            source = event.source.as_deref(),
            termination = ?event.termination,
            detail = event.detail.as_deref(),
            "openab-pty audit"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_serialization_contains_only_a_hash_fingerprint() {
        let event = AuditEvent::new(AuditKind::AdminAuthFailure)
            .fingerprint(hash_fingerprint(&[7; 32]))
            .detail("invalid_credential");
        let rendered = serde_json::to_string(&event).unwrap();
        assert!(rendered.contains("credential_fingerprint"));
        assert!(!rendered.contains("synthetic-admin-credential"));
    }

    #[test]
    fn termination_classes_are_distinct() {
        assert_ne!(TerminationClass::UserKill, TerminationClass::SelfExit);
        assert_ne!(
            TerminationClass::SelfExit,
            TerminationClass::RuntimeShutdown
        );
    }
}

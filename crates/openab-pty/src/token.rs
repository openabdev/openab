//! Keyless attach-token storage.
//!
//! There is intentionally no HMAC/JWT signing key in this crate. A session token
//! is a random opaque capability whose *hash* is kept in memory. Eliminating a
//! signing key eliminates the minting authority a same-container shell could
//! otherwise steal at rest.

use crate::audit::{hash_fingerprint, AuditEvent, AuditKind, AuditLogger};
use crate::containment::SecretBytes;
use crate::{Error, Generation, SessionName};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

/// Short exposure bound for a reusable attach token. It is deliberately much
/// shorter than the default absolute session TTL.
pub const DEFAULT_ATTACH_TOKEN_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenScope {
    AttachOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachGrant {
    pub session: SessionName,
    pub generation: Generation,
    pub scope: TokenScope,
    pub expires_at: Instant,
}

#[derive(Clone)]
struct StoredAttachToken {
    hash: [u8; 32],
    generation: Generation,
    scope: TokenScope,
    expires_at: Instant,
}

/// One-time result of a remote admin create/renew. The plaintext is deliberately
/// an owned zeroizing buffer; it is not stored by the runtime after this return.
pub struct MintedAttachToken {
    pub plaintext: SecretBytes,
    pub grant: AttachGrant,
}

/// Admin/session-side state. Do not give this type to the WebSocket attach
/// handler; [`AttachVerifier`] exposes verification only and cannot mint.
#[derive(Clone)]
pub struct TokenStore {
    entries: Arc<parking_lot::Mutex<HashMap<SessionName, StoredAttachToken>>>,
    ttl: Duration,
    audit: AuditLogger,
}

/// Capability intentionally limited to the attach path. It has no issuance API.
#[derive(Clone)]
pub struct AttachVerifier {
    entries: Arc<parking_lot::Mutex<HashMap<SessionName, StoredAttachToken>>>,
    audit: AuditLogger,
}

impl TokenStore {
    pub fn new(ttl: Duration, audit: AuditLogger) -> Self {
        Self {
            entries: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            ttl,
            audit,
        }
    }

    pub fn with_default_ttl(audit: AuditLogger) -> Self {
        Self::new(DEFAULT_ATTACH_TOKEN_TTL, audit)
    }

    /// Mint from the remote-admin/session path only. The attach verifier below
    /// has no corresponding method, enforcing attach-only verification by API
    /// shape rather than relying on a routing convention.
    pub fn mint_for_session(
        &self,
        session: SessionName,
        generation: Generation,
    ) -> Result<MintedAttachToken, Error> {
        if self.ttl.is_zero() {
            return Err(Error::Other("attach token TTL must be non-zero".into()));
        }
        let mut raw = [0u8; 32];
        OsRng.fill_bytes(&mut raw);

        // Lowercase hex is RFC 6455 subprotocol-token-safe, unlike padded base64.
        let mut encoded = Vec::with_capacity(64);
        for byte in raw {
            encoded.extend_from_slice(format!("{byte:02x}").as_bytes());
        }
        zero_array(&mut raw);

        // Hash the ENCODED form, because that is what a client presents on attach.
        // Hashing the raw bytes here instead would make every verification fail:
        // sha256(raw) != sha256(hex(raw)).
        let hash = sha256(&encoded);

        let expires_at = Instant::now() + self.ttl;
        let grant = AttachGrant {
            session: session.clone(),
            generation,
            scope: TokenScope::AttachOnly,
            expires_at,
        };
        self.entries.lock().insert(
            session,
            StoredAttachToken {
                hash,
                generation,
                scope: TokenScope::AttachOnly,
                expires_at,
            },
        );
        Ok(MintedAttachToken {
            plaintext: SecretBytes::new(encoded),
            grant,
        })
    }

    pub fn attach_verifier(&self) -> AttachVerifier {
        AttachVerifier {
            entries: self.entries.clone(),
            audit: self.audit.clone(),
        }
    }

    /// Kill/restart/renew must call this before exposing a new generation. It
    /// removes the only retained hash, invalidating every old plaintext token.
    pub fn revoke_session(&self, session: &SessionName, generation: Generation) {
        let removed = self.entries.lock().remove(session);
        if let Some(stored) = removed {
            self.audit.record(
                AuditEvent::new(AuditKind::TokenRevoked)
                    .session(session, generation)
                    .fingerprint(hash_fingerprint(&stored.hash)),
            );
        }
    }

    pub fn remove_expired(&self) {
        let now = Instant::now();
        self.entries.lock().retain(|session, stored| {
            let keep = stored.expires_at > now;
            if !keep {
                self.audit.record(
                    AuditEvent::new(AuditKind::TokenExpired)
                        .session(session, stored.generation)
                        .fingerprint(hash_fingerprint(&stored.hash)),
                );
            }
            keep
        });
    }
}

impl AttachVerifier {
    /// Verify an attach capability and erase the supplied plaintext before
    /// returning. Tokens remain reusable until expiry or a generation revocation.
    pub fn verify(
        &self,
        session: &SessionName,
        generation: Generation,
        presented: &mut SecretBytes,
        source: Option<&str>,
    ) -> Result<AttachGrant, Error> {
        let presented_hash = sha256(presented.as_bytes());
        presented.zeroize();

        let now = Instant::now();
        let mut entries = self.entries.lock();
        let outcome = entries.get(session).cloned().filter(|stored| {
            stored.expires_at > now
                && stored.generation == generation
                && stored.scope == TokenScope::AttachOnly
                && bool::from(presented_hash.ct_eq(&stored.hash))
        });

        match outcome {
            Some(stored) => Ok(AttachGrant {
                session: session.clone(),
                generation: stored.generation,
                scope: stored.scope,
                expires_at: stored.expires_at,
            }),
            None => {
                // Expired hashes are removed on observation; no plaintext survives
                // this branch because `presented` was erased before the comparison.
                if entries
                    .get(session)
                    .is_some_and(|stored| stored.expires_at <= now)
                {
                    entries.remove(session);
                }
                drop(entries);
                self.audit.record(
                    AuditEvent::new(AuditKind::AttachAuthFailure)
                        .session(session, generation)
                        .source(source.unwrap_or("unknown"))
                        .detail("invalid_or_expired_attach_token"),
                );
                Err(Error::Unauthorized)
            }
        }
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn zero_array(bytes: &mut [u8; 32]) {
    for byte in bytes {
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionName {
        SessionName::parse("test-session").unwrap()
    }

    #[test]
    fn a_token_can_reattach_until_revoked() {
        let store = TokenStore::new(Duration::from_secs(60), AuditLogger);
        let name = session();
        let minted = store.mint_for_session(name.clone(), Generation(1)).unwrap();
        let token = minted.plaintext.as_bytes().to_vec();
        let verifier = store.attach_verifier();

        for _ in 0..2 {
            let mut presented = SecretBytes::new(token.clone());
            let grant = verifier
                .verify(&name, Generation(1), &mut presented, None)
                .expect("reconnect token remains valid");
            assert_eq!(grant.scope, TokenScope::AttachOnly);
            assert!(presented.as_bytes().iter().all(|byte| *byte == 0));
        }
    }

    #[test]
    fn revocation_and_generation_fence_invalidate_all_old_tokens() {
        let store = TokenStore::new(Duration::from_secs(60), AuditLogger);
        let name = session();
        let minted = store.mint_for_session(name.clone(), Generation(1)).unwrap();
        let token = minted.plaintext.as_bytes().to_vec();
        let verifier = store.attach_verifier();

        let mut wrong_generation = SecretBytes::new(token.clone());
        assert!(verifier
            .verify(&name, Generation(2), &mut wrong_generation, None)
            .is_err());
        store.revoke_session(&name, Generation(2));
        let mut revoked = SecretBytes::new(token);
        assert!(verifier
            .verify(&name, Generation(1), &mut revoked, None)
            .is_err());
    }

    #[test]
    fn minted_token_is_256_bits_encoded_as_safe_hex() {
        let store = TokenStore::new(Duration::from_secs(60), AuditLogger);
        let minted = store.mint_for_session(session(), Generation(0)).unwrap();
        assert_eq!(minted.plaintext.len(), 64);
        assert!(minted
            .plaintext
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
    }
}

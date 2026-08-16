//! `openab-pty` — remote sandboxed terminal sessions.
//!
//! Design of record: `docs/adr/openab-pty-runtime.md`. Phase 1 scope only, and
//! internal/unpublished per the Gate A revision on the tracking issue: no image,
//! no chart, no user docs, no web client.
//!
//! Two invariants from the ADR shape this crate and must not be quietly relaxed:
//!
//! 1. **The admin plane is remote-only, and the credential is its whole
//!    boundary.** No admin socket, UDS, or in-container CLI exists, so no code
//!    path treats being *inside* the container as authorization. That is not the
//!    same as being unreachable, and the ADR no longer claims it is: a managed
//!    session shares this container's network namespace and *can* connect to the
//!    one listener, where it gets `401`. The claim is the weaker, true one — **a
//!    managed session cannot authenticate to, or successfully invoke, admin
//!    operations** — because the credential exists in-container only as a
//!    non-reversible hash, and tokens are never written to a PTY master. Residual
//!    risks of that reachability are stated in the ADR: bounded local DoS from
//!    inside a session, and in-session guessing against a 256-bit CSPRNG value.
//! 2. **Teardown is best-effort.** Tier 1 (process group + subreaper/pidfd) is
//!    the only kill domain implemented, and that must be labelled wherever it is
//!    surfaced. Tier 2 (per-session cgroup) is the ADR's demand-gated plan, is
//!    not implemented here, and `kill_domain_tier = "tier2-required"` is refused
//!    at startup rather than silently downgraded.

pub mod admin_auth;
pub mod audit;
pub mod config;
pub mod containment;
pub mod killdomain;
pub mod ringbuf;
pub mod server;
pub mod session;
pub mod termfilter;
pub mod token;

use std::fmt;

/// Validated session name. The ADR restricts names to `[a-z0-9-]{1,32}` so they
/// are safe in paths, URLs, and log lines.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionName(String);

impl SessionName {
    /// Parse a session name, rejecting anything outside the allowlist.
    pub fn parse(s: &str) -> Result<Self, Error> {
        let ok = !s.is_empty()
            && s.len() <= 32
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if ok {
            Ok(Self(s.to_string()))
        } else {
            Err(Error::InvalidSessionName(s.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Monotonically increasing per-session fence. Bumped by renew, kill, and
/// restart-in-place; every outstanding attach token for the session becomes
/// invalid the moment it changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Generation(pub u64);

impl fmt::Display for Generation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// WebSocket close codes. Distinct codes are a hard requirement from the ADR:
/// clients must be able to distinguish expiry from takeover from a replaced
/// runtime, instead of surfacing an opaque network error.
pub mod close_code {
    /// The detached-idle or absolute TTL elapsed.
    pub const TTL_EXPIRED: u16 = 4001;
    /// Another connection won the single-attach CAS.
    pub const TAKEOVER: u16 = 4002;
    /// An admin `renew` rotated the token; the session process survives.
    pub const RENEWED: u16 = 4003;
    /// The child exited on its own.
    pub const SESSION_ENDED: u16 = 4004;
    /// The client could not drain its backlog past the hard watermark.
    pub const SLOW_CLIENT: u16 = 4005;
    /// The runtime is shutting down: pod or task replaced, workspace reset.
    /// Separate from every code above so clients can say so plainly.
    pub const RUNTIME_REPLACED: u16 = 4006;
    /// Session capacity, tracking capacity, or an admission bound was hit.
    pub const CAPACITY: u16 = 4007;
    /// An operator killed the session through the admin plane. Distinct from
    /// [`TTL_EXPIRED`] because a client that reports "session expired" for a kill
    /// somebody just issued is telling its user something untrue.
    pub const ADMIN_KILL: u16 = 4008;
    /// An internal fault, not a client-correctable condition. Distinct from
    /// [`CAPACITY`], which invites an immediate retry-create.
    pub const INTERNAL_ERROR: u16 = 4009;
}

/// Errors surfaced across module boundaries.
#[derive(Debug)]
pub enum Error {
    InvalidSessionName(String),
    NotFound(SessionName),
    AlreadyExists(SessionName),
    CapacityExceeded {
        limit: usize,
    },
    Unauthorized,
    /// The child is gone; the caller should offer restart-in-place.
    SessionDead(SessionName),
    Config(String),
    Io(std::io::Error),
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSessionName(s) => {
                write!(f, "invalid session name {s:?}: expected [a-z0-9-]{{1,32}}")
            }
            Self::NotFound(n) => write!(f, "no such session: {n}"),
            Self::AlreadyExists(n) => write!(f, "session already exists: {n}"),
            Self::CapacityExceeded { limit } => {
                write!(f, "session capacity exceeded (limit {limit})")
            }
            Self::Unauthorized => write!(f, "unauthorized"),
            Self::SessionDead(n) => write!(f, "session {n} is dead; restart-in-place required"),
            Self::Config(m) => write!(f, "config error: {m}"),
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_name_accepts_allowlist() {
        for ok in ["a", "my-session", "s0", &"x".repeat(32)] {
            assert!(SessionName::parse(ok).is_ok(), "should accept {ok:?}");
        }
    }

    #[test]
    fn session_name_rejects_everything_else() {
        for bad in [
            "",
            &"x".repeat(33),
            "Upper",
            "under_score",
            "dot.dot",
            "sp ace",
            "../escape",
            "sess/../../etc",
            "unicode\u{00e9}",
        ] {
            assert!(SessionName::parse(bad).is_err(), "should reject {bad:?}");
        }
    }
}

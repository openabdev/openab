//! CP-side configuration.
//!
//! Identity binding is the security core: every auth
//! key maps to **immutable claims** (`namespace`, `name`, `type`, optional
//! caps) owned by CP config. Registration frames are verified against these
//! claims — never the other way around. A compromised runtime cannot escalate
//! to another namespace or to `primary` by editing its own config.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;

use crate::proto::AgentType;

#[derive(Debug, Clone, Deserialize)]
pub struct CpConfig {
    /// Bind address. Defaults to loopback: the CP carries bearer
    /// credentials and terminates no TLS itself, so non-loopback binds
    /// require `allow_insecure_bind = true` and a TLS-terminating proxy
    /// (or a private overlay network) in front.
    #[serde(default = "default_listen")]
    pub listen: String,

    /// Explicit opt-in to bind a non-loopback address WITHOUT in-process
    /// TLS. Only set this when a trusted TLS proxy terminates wss:// in
    /// front of the CP, or the network is private (e.g. a tailnet).
    #[serde(default)]
    pub allow_insecure_bind: bool,

    /// Heartbeat interval communicated to runtimes.
    #[serde(default = "default_heartbeat_secs")]
    pub heartbeat_interval_secs: u64,

    /// Lease window: an instance missing heartbeats past this is deregistered
    /// and its in-flight delegations fail with `TARGET_DISCONNECTED`.
    #[serde(default = "default_lease_secs")]
    pub lease_expiry_secs: u64,

    /// Hard cap on delegation deadline length (seconds from now). Deadlines
    /// beyond this are rejected at `cp/delegate`.
    #[serde(default = "default_max_deadline_secs")]
    pub max_deadline_secs: u64,

    /// Maximum result payload size in bytes (`cp/delegate_result.result`).
    /// Oversized results are truncated with a marker, not rejected — the
    /// delegation already ran; losing the tail beats losing everything.
    #[serde(default = "default_max_result_bytes")]
    pub max_result_bytes: usize,

    /// Maximum WebSocket message size accepted from a runtime, enforced by
    /// the transport before any parsing/allocation, so an oversized frame is
    /// never buffered in full.
    #[serde(default = "default_max_frame_bytes")]
    pub max_frame_bytes: usize,

    /// Maximum `cp/delegate.prompt` size in bytes; oversized prompts are
    /// rejected (unlike results, nothing has run yet).
    #[serde(default = "default_max_prompt_bytes")]
    pub max_prompt_bytes: usize,

    /// Deadline for the mandatory `cp/register` first frame, in seconds from
    /// the completed WebSocket upgrade. A connection that authenticates but
    /// never registers is closed when this elapses: otherwise an
    /// authenticated peer could park unlimited sockets in the
    /// pre-registration state, keeping them alive with pings forever.
    #[serde(default = "default_register_timeout_secs")]
    pub register_timeout_secs: u64,

    /// Maximum simultaneous connections per identity, counted from the
    /// upgrade (so pre-registration sockets count too) and released on every
    /// exit path. Authentication alone is not a bound: one leaked key could
    /// otherwise open unlimited sockets. Replicas of one logical agent share
    /// one identity, so this is the replica ceiling as well.
    #[serde(default = "default_max_connections_per_identity")]
    pub max_connections_per_identity: u32,

    /// Identity table: auth key → immutable claims.
    /// Keyed by the key id (`kid`), with the secret alongside, so logs can
    /// reference identities without printing secrets.
    #[serde(default)]
    pub agents: Vec<AgentIdentity>,

    /// Per-namespace policy overrides.
    #[serde(default)]
    pub namespaces: BTreeMap<String, NamespacePolicy>,
}

fn default_listen() -> String {
    "127.0.0.1:9800".to_string()
}
fn default_heartbeat_secs() -> u64 {
    15
}
fn default_lease_secs() -> u64 {
    45
}
fn default_max_deadline_secs() -> u64 {
    30 * 60
}
fn default_max_result_bytes() -> usize {
    256 * 1024
}
fn default_max_frame_bytes() -> usize {
    1024 * 1024
}
fn default_max_prompt_bytes() -> usize {
    256 * 1024
}
fn default_register_timeout_secs() -> u64 {
    10
}
fn default_max_connections_per_identity() -> u32 {
    8
}

/// Immutable identity claims bound to one auth key.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentIdentity {
    /// The secret presented by the runtime (`OPENAB_CP_KEY`). Supports
    /// `${ENV_VAR}` expansion so the config file itself holds no secrets.
    pub key: String,
    pub namespace: String,
    pub name: String,
    #[serde(rename = "type")]
    pub agent_type: AgentType,
    /// Optional CP-side clamp on the advertised concurrency budget.
    #[serde(default)]
    pub max_delegated_sessions_cap: Option<u32>,
}

/// Per-namespace delegation policy. Defaults are the conservative ADR §5
/// baseline; relaxation is CP-side config only.
#[derive(Debug, Clone, Deserialize)]
pub struct NamespacePolicy {
    /// Maximum delegation chain depth (1 = primary → worker only).
    #[serde(default = "default_depth")]
    pub max_depth: u32,
    /// Whether workers may initiate delegations (depth still applies).
    #[serde(default)]
    pub allow_worker_initiation: bool,
}

fn default_depth() -> u32 {
    1
}

impl Default for NamespacePolicy {
    fn default() -> Self {
        Self {
            max_depth: default_depth(),
            allow_worker_initiation: false,
        }
    }
}

impl CpConfig {
    pub fn load(path: &str) -> Result<Self> {
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading CP config {path}"))?;
        let expanded = expand_env(&raw);
        let cfg: CpConfig = toml::from_str(&expanded).context("parsing CP config")?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        let mut seen_keys = std::collections::BTreeSet::new();
        let mut seen_names = std::collections::BTreeSet::new();
        for a in &self.agents {
            if a.key.trim().is_empty() {
                bail!("agent {}/{} has an empty key", a.namespace, a.name);
            }
            if !seen_keys.insert(a.key.as_str()) {
                bail!(
                    "duplicate auth key (shared keys defeat per-agent revocation); \
                     offending identity: {}/{}",
                    a.namespace,
                    a.name
                );
            }
            if !seen_names.insert((a.namespace.as_str(), a.name.as_str())) {
                bail!(
                    "duplicate identity {}/{} — replicas share one identity (one key), \
                     distinguished at registration by instance_id",
                    a.namespace,
                    a.name
                );
            }
        }
        if self.lease_expiry_secs <= self.heartbeat_interval_secs {
            bail!("lease_expiry_secs must exceed heartbeat_interval_secs");
        }
        // Admission bounds must actually bound something:
        // zero would mean "no registration deadline" / "no connection allowed".
        if self.register_timeout_secs == 0 {
            bail!("register_timeout_secs must be greater than 0");
        }
        if self.max_connections_per_identity == 0 {
            bail!("max_connections_per_identity must be at least 1");
        }
        // Bearer keys over cleartext TCP must never reach an untrusted
        // network: non-loopback binds require the explicit override.
        if !self.allow_insecure_bind && !is_loopback(&self.listen) {
            bail!(
                "listen = \"{}\" is not loopback and the CP terminates no TLS. \
                 Put a TLS proxy (wss://) or a private network in front and set \
                 allow_insecure_bind = true to acknowledge this",
                self.listen
            );
        }
        Ok(())
    }

    /// Constant-time lookup of the identity bound to `key`.
    pub fn identity_for_key(&self, key: &str) -> Option<&AgentIdentity> {
        use subtle::ConstantTimeEq;
        // Compare against every entry to avoid early-exit timing signal on
        // which identity matched.
        let mut found: Option<&AgentIdentity> = None;
        for a in &self.agents {
            let eq: bool = a.key.as_bytes().ct_eq(key.as_bytes()).into();
            if eq {
                found = Some(a);
            }
        }
        found
    }

    pub fn policy_for(&self, namespace: &str) -> NamespacePolicy {
        self.namespaces.get(namespace).cloned().unwrap_or_default()
    }
}

/// Whether a `host:port` bind address is loopback.
fn is_loopback(listen: &str) -> bool {
    let host = match listen.rsplit_once(':') {
        Some((h, _)) => h.trim_start_matches('[').trim_end_matches(']'),
        None => listen,
    };
    if host == "localhost" {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// `${ENV_VAR}` expansion, mirroring openab-core config behavior. Unset vars
/// expand to the empty string (validation then rejects empty keys loudly).
fn expand_env(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find('}') {
            Some(end) => {
                let var = &rest[start + 2..start + 2 + end];
                out.push_str(&std::env::var(var).unwrap_or_default());
                rest = &rest[start + 2 + end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_toml() -> &'static str {
        r#"
listen = "127.0.0.1:9800"

[[agents]]
key = "k-primary"
namespace = "prod"
name = "koudu"
type = "primary"

[[agents]]
key = "k-worker"
namespace = "prod"
name = "worker-1"
type = "worker"
max_delegated_sessions_cap = 2

[namespaces.prod]
max_depth = 2
allow_worker_initiation = false
"#
    }

    #[test]
    fn parses_and_validates() {
        let cfg: CpConfig = toml::from_str(base_toml()).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.agents.len(), 2);
        assert_eq!(cfg.policy_for("prod").max_depth, 2);
        // unknown namespace falls back to conservative defaults
        let d = cfg.policy_for("dev");
        assert_eq!(d.max_depth, 1);
        assert!(!d.allow_worker_initiation);
    }

    #[test]
    fn identity_lookup_binds_key_to_claims() {
        let cfg: CpConfig = toml::from_str(base_toml()).unwrap();
        let id = cfg.identity_for_key("k-worker").unwrap();
        assert_eq!(id.name, "worker-1");
        assert_eq!(id.agent_type, AgentType::Worker);
        assert!(cfg.identity_for_key("k-unknown").is_none());
    }

    #[test]
    fn rejects_duplicate_keys() {
        let toml_str = r#"
[[agents]]
key = "same"
namespace = "prod"
name = "a"
type = "primary"

[[agents]]
key = "same"
namespace = "prod"
name = "b"
type = "worker"
"#;
        let cfg: CpConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_lease_not_exceeding_heartbeat() {
        let toml_str = r#"
heartbeat_interval_secs = 30
lease_expiry_secs = 30
"#;
        let cfg: CpConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn non_loopback_bind_requires_override() {
        let cfg: CpConfig = toml::from_str("listen = \"0.0.0.0:9800\"").unwrap();
        assert!(cfg.validate().is_err());
        let cfg: CpConfig =
            toml::from_str("listen = \"0.0.0.0:9800\"\nallow_insecure_bind = true").unwrap();
        cfg.validate().unwrap();
        // Loopback variants pass without the override.
        for l in ["127.0.0.1:9800", "localhost:9800", "[::1]:9800"] {
            let cfg: CpConfig = toml::from_str(&format!("listen = \"{l}\"")).unwrap();
            cfg.validate().unwrap();
        }
    }

    #[test]
    fn admission_bounds_default_and_are_validated() {
        // Absent fields keep working (serde defaults) and
        // a zero bound is rejected rather than silently disabling the guard.
        let cfg: CpConfig = toml::from_str(base_toml()).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.register_timeout_secs, 10);
        assert_eq!(cfg.max_connections_per_identity, 8);

        let explicit: CpConfig =
            toml::from_str("register_timeout_secs = 3\nmax_connections_per_identity = 2").unwrap();
        explicit.validate().unwrap();
        assert_eq!(explicit.register_timeout_secs, 3);
        assert_eq!(explicit.max_connections_per_identity, 2);

        for bad in [
            "register_timeout_secs = 0",
            "max_connections_per_identity = 0",
        ] {
            let cfg: CpConfig = toml::from_str(bad).unwrap();
            assert!(cfg.validate().is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn env_expansion() {
        std::env::set_var("CP_TEST_KEY_XYZ", "sekrit");
        assert_eq!(
            expand_env("key = \"${CP_TEST_KEY_XYZ}\""),
            "key = \"sekrit\""
        );
        assert_eq!(expand_env("no vars"), "no vars");
        assert_eq!(expand_env("${UNSET_VAR_ABC123}"), "");
    }
}

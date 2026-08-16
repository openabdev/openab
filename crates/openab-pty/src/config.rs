//! Validation of the delivered, least-privilege `[pty]` configuration projection.
//!
//! This crate never resolves cloud secrets. The projection is materialized outside
//! the PTY trust boundary, and the validator rejects any evidence that a broker
//! config, cloud reference, or unresolved `${secrets.*}` value leaked into it.
//!
//! The projection is deliberately **small**: only values the ADR names as
//! operator-facing behaviour are knobs. Liveness, takeover, and teardown-grace
//! values are constants in [`crate::session`] — nobody asked to configure them
//! and no telemetry exists to tune them, so an unknown `[pty]` key is a startup
//! error rather than a silently-ignored one.

use crate::admin_auth::parse_verifier_hash;
use crate::session::{MISSED_PINGS_BEFORE_DETACHED, PING_INTERVAL, TTL_WARNING_LEAD};
use crate::Error;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use toml::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillDomainRequirement {
    Tier1Allowed,
    /// Retained so an operator who requires the hard guarantee is refused at
    /// startup. Tier 2 is not implemented — see [`crate::killdomain`].
    Tier2Required,
}

impl KillDomainRequirement {
    /// The `kill_domain_tier` token that produces this value.
    ///
    /// Surfaces (`--validate-projection`, logs) must print *this*, not the Debug
    /// name: `Tier1Allowed` is not accepted by [`parse_kill_domain`], so printing
    /// it hands the operator a value their config will reject.
    pub fn as_config_value(self) -> &'static str {
        match self {
            Self::Tier1Allowed => "tier1",
            Self::Tier2Required => "tier2-required",
        }
    }
}

#[derive(Clone, Debug)]
pub struct PtyConfig {
    pub enabled: bool,
    pub listen: String,
    pub tls_terminated_upstream: bool,
    pub command: PathBuf,
    pub max_sessions: usize,
    pub absolute_session_ttl: Duration,
    pub detached_idle_ttl: Duration,
    pub scrollback_kib: usize,
    pub scrollback_replay: bool,
    /// Literal `sha256:<64 lowercase hex>`; retained as a verifier, never a credential.
    pub admin_credential_hash: String,
    pub kill_domain_requirement: KillDomainRequirement,
}

const PTY_KEYS: &[&str] = &[
    "enabled",
    "listen",
    "tls_terminated_upstream",
    "command",
    "max_sessions",
    "absolute_session_ttl",
    "detached_idle_ttl",
    "scrollback_kib",
    "scrollback_replay",
    "admin_credential_hash",
    "kill_domain_tier",
];

/// Parse and fail-closed validate a delivered PTY projection.
///
/// This is intentionally reusable by startup, `--validate-projection`, and CI
/// guard tests. It accepts literals and `${VAR}` expansion only; no cloud resolver
/// is linked or invoked here. `admin_credential_hash` is stricter: it is literal
/// only, so an unset or poisoned env expression can never disable admin auth.
pub fn validate_projection(input: &str) -> Result<PtyConfig, Error> {
    let mut root: Value = input
        .parse::<Value>()
        .map_err(|error| Error::Config(format!("invalid TOML projection: {error}")))?;
    reject_unsafe_values(&root)?;

    let root_table = root
        .as_table_mut()
        .ok_or_else(|| Error::Config("PTY projection root must be a TOML table".into()))?;
    if root_table.contains_key("secrets") {
        return Err(Error::Config(
            "[secrets.refs] is forbidden in a delivered PTY projection".into(),
        ));
    }
    let sections: BTreeSet<&str> = root_table.keys().map(String::as_str).collect();
    if sections != BTreeSet::from(["pty"]) {
        return Err(Error::Config(format!(
            "PTY projection may contain only [pty], found [{}]; broker sections and shared source config are forbidden",
            sections.into_iter().collect::<Vec<_>>().join(", ")
        )));
    }

    let pty = root_table
        .get_mut("pty")
        .and_then(Value::as_table_mut)
        .ok_or_else(|| Error::Config("[pty] must be a table".into()))?;
    for key in pty.keys() {
        if !PTY_KEYS.contains(&key.as_str()) {
            return Err(Error::Config(format!("unknown [pty] key {key:?}")));
        }
    }

    // The verifier is checked before interpolation. A fail-open here would let a
    // poisoned projection turn off remote admin authentication entirely.
    let raw_hash = required_string(pty, "admin_credential_hash")?.to_owned();
    if raw_hash.is_empty() || raw_hash.contains("${") {
        return Err(Error::Config(
            "admin_credential_hash must be a non-empty literal verifier hash; interpolation is forbidden"
                .into(),
        ));
    }
    parse_verifier_hash(&raw_hash)?;

    interpolate_table(pty)?;
    let config = PtyConfig {
        enabled: bool_with_default(pty, "enabled", false)?,
        listen: string_with_default(pty, "listen", "0.0.0.0:8090")?.to_owned(),
        tls_terminated_upstream: bool_with_default(pty, "tls_terminated_upstream", true)?,
        command: PathBuf::from(string_with_default(pty, "command", "/bin/bash")?),
        max_sessions: usize_with_default(pty, "max_sessions", 4)?,
        absolute_session_ttl: duration_with_default(pty, "absolute_session_ttl", "12h")?,
        detached_idle_ttl: duration_with_default(pty, "detached_idle_ttl", "30m")?,
        scrollback_kib: usize_with_default(pty, "scrollback_kib", 1024)?,
        scrollback_replay: bool_with_default(pty, "scrollback_replay", false)?,
        admin_credential_hash: raw_hash,
        kill_domain_requirement: parse_kill_domain(string_with_default(
            pty,
            "kill_domain_tier",
            "tier1",
        )?)?,
    };
    validate_lifecycle(&config)?;
    validate_admission(&config)?;
    Ok(config)
}

/// Read a local delivered projection and run the same validation used at startup.
pub fn validate_projection_file(path: impl AsRef<Path>) -> Result<PtyConfig, Error> {
    let content = std::fs::read_to_string(path)?;
    validate_projection(&content)
}

fn reject_unsafe_values(value: &Value) -> Result<(), Error> {
    match value {
        Value::String(value) => {
            if value.contains("${secrets.") {
                return Err(Error::Config(
                    "${secrets.*} interpolation is forbidden in a PTY projection".into(),
                ));
            }
            if value.contains("://") {
                return Err(Error::Config(
                    "remote/cloud URI references are forbidden in a PTY projection".into(),
                ));
            }
            Ok(())
        }
        Value::Array(values) => values.iter().try_for_each(reject_unsafe_values),
        Value::Table(values) => values.values().try_for_each(reject_unsafe_values),
        _ => Ok(()),
    }
}

fn interpolate_table(table: &mut toml::map::Map<String, Value>) -> Result<(), Error> {
    for (key, value) in table {
        if key == "admin_credential_hash" {
            continue;
        }
        if let Value::String(text) = value {
            *text = interpolate_env(text)?;
        }
    }
    Ok(())
}

fn interpolate_env(input: &str) -> Result<String, Error> {
    let mut output = String::with_capacity(input.len());
    let mut remainder = input;
    while let Some(start) = remainder.find("${") {
        output.push_str(&remainder[..start]);
        let after_open = &remainder[start + 2..];
        let Some(end) = after_open.find('}') else {
            return Err(Error::Config("unterminated ${VAR} interpolation".into()));
        };
        let name = &after_open[..end];
        if !is_env_name(name) {
            return Err(Error::Config(format!(
                "invalid interpolation ${{{name}}}; only ${{VAR}} environment values are allowed"
            )));
        }
        let value = std::env::var(name).map_err(|_| {
            Error::Config(format!(
                "environment variable {name:?} required by PTY projection is unset"
            ))
        })?;
        if value.contains("://") || value.contains("${secrets.") {
            return Err(Error::Config(format!(
                "environment value {name:?} resolves to a forbidden remote/cloud reference"
            )));
        }
        output.push_str(&value);
        remainder = &after_open[end + 1..];
    }
    output.push_str(remainder);
    Ok(output)
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.bytes();
    matches!(chars.next(), Some(b) if b.is_ascii_alphabetic() || b == b'_')
        && chars.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn required_string<'a>(
    table: &'a toml::map::Map<String, Value>,
    key: &str,
) -> Result<&'a str, Error> {
    table
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Config(format!("[pty].{key} must be a string and is required")))
}

fn string_with_default<'a>(
    table: &'a toml::map::Map<String, Value>,
    key: &str,
    default: &'a str,
) -> Result<&'a str, Error> {
    match table.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_str()
            .ok_or_else(|| Error::Config(format!("[pty].{key} must be a string"))),
    }
}

fn bool_with_default(
    table: &toml::map::Map<String, Value>,
    key: &str,
    default: bool,
) -> Result<bool, Error> {
    match table.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| Error::Config(format!("[pty].{key} must be a boolean"))),
    }
}

fn usize_with_default(
    table: &toml::map::Map<String, Value>,
    key: &str,
    default: usize,
) -> Result<usize, Error> {
    match table.get(key).and_then(Value::as_integer) {
        None if !table.contains_key(key) => Ok(default),
        Some(value) if value >= 0 => {
            usize::try_from(value).map_err(|_| Error::Config(format!("[pty].{key} is too large")))
        }
        _ => Err(Error::Config(format!(
            "[pty].{key} must be a non-negative integer"
        ))),
    }
}

fn duration_with_default(
    table: &toml::map::Map<String, Value>,
    key: &str,
    default: &str,
) -> Result<Duration, Error> {
    parse_duration(key, string_with_default(table, key, default)?)
}

fn parse_duration(key: &str, value: &str) -> Result<Duration, Error> {
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| Error::Config(format!("{key} needs a unit (s, m, h, d)")))?;
    let (number, unit) = value.split_at(split);
    let amount: u64 = number
        .parse()
        .map_err(|_| Error::Config(format!("{key} must be a positive integer duration")))?;
    if amount == 0 {
        return Err(Error::Config(format!("{key} must be greater than zero")));
    }
    let seconds = match unit {
        "s" => Some(amount),
        "m" => amount.checked_mul(60),
        "h" => amount.checked_mul(60 * 60),
        "d" => amount.checked_mul(24 * 60 * 60),
        _ => return Err(Error::Config(format!("{key} unit must be s, m, h, or d"))),
    }
    .ok_or_else(|| Error::Config(format!("{key} overflows")))?;
    Ok(Duration::from_secs(seconds))
}

fn parse_kill_domain(value: &str) -> Result<KillDomainRequirement, Error> {
    match value {
        "tier1" => Ok(KillDomainRequirement::Tier1Allowed),
        "tier2-required" => Ok(KillDomainRequirement::Tier2Required),
        _ => Err(Error::Config(
            "kill_domain_tier must be tier1 or tier2-required".into(),
        )),
    }
}

/// Cross-field checks between the two configurable TTLs and the fixed liveness
/// constants. A projection whose idle TTL fires before a half-open socket can be
/// detected, or before the client-visible warning can be sent, is a startup
/// error rather than a liveness model nobody reviewed.
fn validate_lifecycle(config: &PtyConfig) -> Result<(), Error> {
    let detach_after = PING_INTERVAL
        .checked_mul(MISSED_PINGS_BEFORE_DETACHED)
        .ok_or_else(|| Error::Config("liveness constants overflow".into()))?;
    if detach_after >= config.detached_idle_ttl {
        return Err(Error::Config(
            "detached_idle_ttl must be longer than the fixed ping interval × missed-ping budget \
             so half-open sockets detach before idle teardown"
                .into(),
        ));
    }
    if TTL_WARNING_LEAD >= config.detached_idle_ttl {
        return Err(Error::Config(
            "detached_idle_ttl must be longer than the fixed TTL warning lead so the expiry \
             warning precedes teardown"
                .into(),
        ));
    }
    if config.absolute_session_ttl < config.detached_idle_ttl {
        return Err(Error::Config(
            "absolute_session_ttl must not be shorter than detached_idle_ttl".into(),
        ));
    }
    Ok(())
}

fn validate_admission(config: &PtyConfig) -> Result<(), Error> {
    if config.max_sessions == 0 {
        return Err(Error::Config("max_sessions must be non-zero".into()));
    }
    if config.command.as_os_str().is_empty() || !config.command.is_absolute() {
        return Err(Error::Config(
            "command must be a non-empty absolute local path".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn valid() -> String {
        format!(
            r#"[pty]
enabled = true
listen = "0.0.0.0:8090"
tls_terminated_upstream = true
command = "/bin/bash"
max_sessions = 4
absolute_session_ttl = "12h"
scrollback_kib = 1024
scrollback_replay = false
admin_credential_hash = "{HASH}"
"#
        )
    }

    #[test]
    fn accepts_materialized_projection() {
        let config = validate_projection(&valid()).unwrap();
        assert_eq!(config.max_sessions, 4);
        assert_eq!(config.detached_idle_ttl, Duration::from_secs(30 * 60));
        assert_eq!(
            config.kill_domain_requirement,
            KillDomainRequirement::Tier1Allowed
        );
    }

    #[test]
    fn the_two_configurable_ttls_round_trip_into_session_policy() {
        let config = validate_projection(&format!("{}detached_idle_ttl = \"40m\"\n", valid()))
            .expect("both TTLs are operator-facing");
        let policy = crate::session::SessionPolicy::from_config(&config);
        assert_eq!(policy.absolute_ttl, config.absolute_session_ttl);
        assert_eq!(policy.detached_idle_ttl, Duration::from_secs(40 * 60));
        // Everything else is a constant, not a knob.
        assert_eq!(policy.ping_interval, PING_INTERVAL);
        assert_eq!(
            policy.missed_pings_before_detached,
            MISSED_PINGS_BEFORE_DETACHED
        );
        assert_eq!(policy.ttl_warning_lead, TTL_WARNING_LEAD);
        assert_eq!(
            policy.max_takeovers_per_window,
            crate::session::MAX_TAKEOVERS_PER_WINDOW
        );
        assert_eq!(policy.takeover_window, crate::session::TAKEOVER_WINDOW);
        assert_eq!(policy.teardown_grace, crate::session::TEARDOWN_GRACE);
    }

    #[test]
    fn withdrawn_knobs_are_rejected_as_unknown_keys() {
        // The liveness/admission values are constants now. An operator who still
        // sets one must be told, not silently ignored.
        for key in [
            "ping_interval = \"15s\"",
            "missed_pings_before_detached = 2",
            "ttl_warning_lead = \"20s\"",
            "max_takeovers_per_window = 7",
            "takeover_window = \"2m\"",
            "teardown_grace = \"9s\"",
            "replay_queue_kib = 256",
            "outbound_backlog_kib = 256",
            "fixed_session_overhead_kib = 512",
            "runtime_baseline_kib = 16384",
            "container_memory_request_kib = 32768",
            "per_session_disk_kib = 1024",
            "volume_capacity_kib = 1024",
            "volume_overhead_kib = 1024",
        ] {
            let error = validate_projection(&format!("{}{key}\n", valid()))
                .expect_err("a withdrawn knob must fail closed");
            assert!(
                error.to_string().contains("unknown [pty] key"),
                "{key}: {error}"
            );
        }
    }

    #[test]
    fn ttl_values_are_rejected_when_out_of_range_or_incompatible() {
        for (projection, constraint) in [
            (
                format!("{}detached_idle_ttl = \"0s\"\n", valid()),
                "greater than zero",
            ),
            (
                format!("{}detached_idle_ttl = \"30s\"\n", valid()),
                "half-open sockets detach before idle teardown",
            ),
            (
                valid().replace(
                    "absolute_session_ttl = \"12h\"",
                    "absolute_session_ttl = \"29m\"",
                ),
                "must not be shorter than detached_idle_ttl",
            ),
        ] {
            let error =
                validate_projection(&projection).expect_err("incompatible TTLs must fail closed");
            assert!(error.to_string().contains(constraint), "{error}");
        }
    }

    #[test]
    fn rejects_secret_table_cloud_references_and_broker_sections() {
        for poisoned in [
            format!("[secrets.refs]\nx = \"aws-sm://x\"\n{}", valid()),
            valid().replace("/bin/bash", "aws-sm://openab/pty#command"),
            format!("[discord]\nbot_token = \"x\"\n{}", valid()),
        ] {
            assert!(
                validate_projection(&poisoned).is_err(),
                "must fail closed: {poisoned}"
            );
        }
    }

    #[test]
    fn rejects_both_verifier_poisoning_cases() {
        let interpolation = valid().replace(HASH, "${secrets.pty_admin_hash}");
        assert!(
            validate_projection(&interpolation).is_err(),
            "secrets interpolation must fail"
        );
        let env_interpolation = valid().replace(HASH, "${PTY_ADMIN_HASH}");
        assert!(
            validate_projection(&env_interpolation).is_err(),
            "verifier env interpolation must fail"
        );
        let malformed = valid().replace(HASH, "sha256:not-a-verifier");
        assert!(
            validate_projection(&malformed).is_err(),
            "malformed verifier must fail"
        );
        let empty = valid().replace(HASH, "");
        assert!(
            validate_projection(&empty).is_err(),
            "empty verifier must fail"
        );
    }

    #[test]
    fn accepts_explicit_tier_two_requirement_so_startup_can_refuse_it() {
        // Parsing succeeds; `killdomain::resolve_tier` is what refuses, loudly.
        let config = validate_projection(&format!(
            "{}kill_domain_tier = \"tier2-required\"\n",
            valid()
        ))
        .unwrap();
        assert_eq!(
            config.kill_domain_requirement,
            KillDomainRequirement::Tier2Required
        );
        assert!(crate::killdomain::resolve_tier(config.kill_domain_requirement).is_err());
    }

    #[test]
    fn the_printed_tier_vocabulary_is_the_one_config_accepts() {
        // `--validate-projection` used to print the Debug name (`Tier1Allowed`),
        // which `parse_kill_domain` rejects — so copying the validator's own
        // output into a projection produced an invalid projection.
        for requirement in [
            KillDomainRequirement::Tier1Allowed,
            KillDomainRequirement::Tier2Required,
        ] {
            assert_eq!(
                parse_kill_domain(requirement.as_config_value()).unwrap(),
                requirement
            );
        }
    }
}

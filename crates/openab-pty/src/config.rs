//! Validation of the delivered, least-privilege `[pty]` configuration projection.
//!
//! This crate never resolves cloud secrets. The projection is materialized outside
//! the PTY trust boundary, and the validator rejects any evidence that a broker
//! config, cloud reference, or unresolved `${secrets.*}` value leaked into it.

use crate::admin_auth::parse_verifier_hash;
use crate::Error;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use toml::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KillDomainRequirement {
    Tier1Allowed,
    Tier2Required,
}

#[derive(Clone, Debug)]
pub struct PtyConfig {
    pub enabled: bool,
    pub listen: String,
    pub tls_terminated_upstream: bool,
    pub command: PathBuf,
    pub max_sessions: usize,
    pub absolute_session_ttl: Duration,
    pub scrollback_kib: usize,
    pub scrollback_replay: bool,
    /// Literal `sha256:<64 lowercase hex>`; retained as a verifier, never a credential.
    pub admin_credential_hash: String,
    pub replay_queue_kib: usize,
    pub outbound_backlog_kib: usize,
    pub fixed_session_overhead_kib: usize,
    pub runtime_baseline_kib: usize,
    pub container_memory_request_kib: usize,
    pub per_session_disk_kib: usize,
    pub volume_capacity_kib: usize,
    pub volume_overhead_kib: usize,
    pub kill_domain_requirement: KillDomainRequirement,
}

impl PtyConfig {
    pub fn per_session_memory_kib(&self) -> Result<usize, Error> {
        self.scrollback_kib
            .checked_add(self.replay_queue_kib)
            .and_then(|v| v.checked_add(self.outbound_backlog_kib))
            .and_then(|v| v.checked_add(self.fixed_session_overhead_kib))
            .ok_or_else(|| Error::Config("per-session memory budget overflows usize".into()))
    }

    pub fn required_memory_kib(&self) -> Result<usize, Error> {
        self.max_sessions
            .checked_mul(self.per_session_memory_kib()?)
            .and_then(|v| v.checked_add(self.runtime_baseline_kib))
            .ok_or_else(|| Error::Config("total memory admission formula overflows usize".into()))
    }

    pub fn required_disk_kib(&self) -> Result<usize, Error> {
        self.max_sessions
            .checked_mul(self.per_session_disk_kib)
            .and_then(|v| v.checked_add(self.volume_overhead_kib))
            .ok_or_else(|| Error::Config("total disk admission formula overflows usize".into()))
    }
}

const PTY_KEYS: &[&str] = &[
    "enabled",
    "listen",
    "tls_terminated_upstream",
    "command",
    "max_sessions",
    "absolute_session_ttl",
    "scrollback_kib",
    "scrollback_replay",
    "admin_credential_hash",
    "replay_queue_kib",
    "outbound_backlog_kib",
    "fixed_session_overhead_kib",
    "runtime_baseline_kib",
    "container_memory_request_kib",
    "per_session_disk_kib",
    "volume_capacity_kib",
    "volume_overhead_kib",
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
        absolute_session_ttl: parse_duration(string_with_default(
            pty,
            "absolute_session_ttl",
            "12h",
        )?)?,
        scrollback_kib: usize_with_default(pty, "scrollback_kib", 1024)?,
        scrollback_replay: bool_with_default(pty, "scrollback_replay", false)?,
        admin_credential_hash: raw_hash,
        replay_queue_kib: usize_with_default(pty, "replay_queue_kib", 256)?,
        outbound_backlog_kib: usize_with_default(pty, "outbound_backlog_kib", 256)?,
        fixed_session_overhead_kib: usize_with_default(pty, "fixed_session_overhead_kib", 512)?,
        runtime_baseline_kib: usize_with_default(pty, "runtime_baseline_kib", 16 * 1024)?,
        container_memory_request_kib: usize_with_default(
            pty,
            "container_memory_request_kib",
            32 * 1024,
        )?,
        per_session_disk_kib: usize_with_default(pty, "per_session_disk_kib", 1024 * 1024)?,
        volume_capacity_kib: usize_with_default(pty, "volume_capacity_kib", 5 * 1024 * 1024)?,
        volume_overhead_kib: usize_with_default(pty, "volume_overhead_kib", 1024 * 1024)?,
        kill_domain_requirement: parse_kill_domain(string_with_default(
            pty,
            "kill_domain_tier",
            "tier1",
        )?)?,
    };
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

fn parse_duration(value: &str) -> Result<Duration, Error> {
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| Error::Config("absolute_session_ttl needs a unit (s, m, h, d)".into()))?;
    let (number, unit) = value.split_at(split);
    let amount: u64 = number
        .parse()
        .map_err(|_| Error::Config("absolute_session_ttl has an invalid number".into()))?;
    if amount == 0 {
        return Err(Error::Config(
            "absolute_session_ttl must be non-zero".into(),
        ));
    }
    let seconds = match unit {
        "s" => Some(amount),
        "m" => amount.checked_mul(60),
        "h" => amount.checked_mul(60 * 60),
        "d" => amount.checked_mul(24 * 60 * 60),
        _ => {
            return Err(Error::Config(
                "absolute_session_ttl unit must be s, m, h, or d".into(),
            ))
        }
    }
    .ok_or_else(|| Error::Config("absolute_session_ttl overflows".into()))?;
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

fn validate_admission(config: &PtyConfig) -> Result<(), Error> {
    if config.max_sessions == 0 {
        return Err(Error::Config("max_sessions must be non-zero".into()));
    }
    if config.command.as_os_str().is_empty() || !config.command.is_absolute() {
        return Err(Error::Config(
            "command must be a non-empty absolute local path".into(),
        ));
    }
    let memory = config.required_memory_kib()?;
    if memory > config.container_memory_request_kib {
        return Err(Error::Config(format!(
            "memory admission failed: {memory} KiB required exceeds container_memory_request_kib {} KiB",
            config.container_memory_request_kib
        )));
    }
    let disk = config.required_disk_kib()?;
    if disk > config.volume_capacity_kib {
        return Err(Error::Config(format!(
            "disk admission failed: {disk} KiB required exceeds volume_capacity_kib {} KiB",
            config.volume_capacity_kib
        )));
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
    fn accepts_materialized_projection_and_applies_admission_formula() {
        let config = validate_projection(&valid()).unwrap();
        assert_eq!(config.required_memory_kib().unwrap(), 24 * 1024);
        assert_eq!(
            config.kill_domain_requirement,
            KillDomainRequirement::Tier1Allowed
        );
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
    fn rejects_oversubscribed_memory_and_disk() {
        let memory = format!("{}container_memory_request_kib = 1\n", valid());
        assert!(validate_projection(&memory).is_err());
        let disk = format!("{}volume_capacity_kib = 1\n", valid());
        assert!(validate_projection(&disk).is_err());
    }

    #[test]
    fn accepts_explicit_tier_two_requirement() {
        let config = validate_projection(&format!(
            "{}kill_domain_tier = \"tier2-required\"\n",
            valid()
        ))
        .unwrap();
        assert_eq!(
            config.kill_domain_requirement,
            KillDomainRequirement::Tier2Required
        );
    }
}

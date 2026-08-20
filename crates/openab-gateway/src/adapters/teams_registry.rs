use super::teams_ingress::TeamsIngressRoute;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

const REGISTRY_SCHEMA: &str = "openab.teams.conversation_registry.v1";
const REGISTRY_VERSION: u32 = 1;
pub(super) const DEFAULT_CONVERSATION_REGISTRY_MAX_ENTRIES: usize = 1_000;
pub(super) const DEFAULT_CONVERSATION_REGISTRY_TTL_SECS: u64 = 365 * 24 * 60 * 60;
const REGISTRY_FILE_MAX_BYTES: u64 = 16 * 1024 * 1024;
const REGISTRY_MAX_CONFIGURED_ENTRIES: usize = 10_000;
const REGISTRY_TEMP_MARKER: &str = ".tmp-";
const FIELD_LIMIT: usize = 256;
const ROUTE_ID_LIMIT: usize = 2_048;
const SERVICE_URL_LIMIT: usize = 4_096;
const FORBIDDEN_DISABLE_THRESHOLD: u8 = 2;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TeamsConversationKey {
    pub(super) app_id: String,
    pub(super) tenant_id: String,
    pub(super) bot_framework_channel_id: String,
    pub(super) conversation_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TeamsConversationState {
    Active,
    Disabled,
    Revoked,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TeamsConversationEntry {
    schema_version: u32,
    #[serde(flatten)]
    pub(super) key: TeamsConversationKey,
    pub(super) conversation_type: String,
    pub(super) service_url: String,
    pub(super) team_id: Option<String>,
    pub(super) channel_id: Option<String>,
    pub(super) last_validated_at: DateTime<Utc>,
    pub(super) updated_at: DateTime<Utc>,
    pub(super) state: TeamsConversationState,
    pub(super) reason_code: Option<String>,
    pub(super) consecutive_forbidden_writes: u8,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TeamsConversationRegistryFile {
    schema: String,
    version: u32,
    generation: u64,
    entries: Vec<TeamsConversationEntry>,
}

impl Default for TeamsConversationRegistryFile {
    fn default() -> Self {
        Self {
            schema: REGISTRY_SCHEMA.into(),
            version: REGISTRY_VERSION,
            generation: 0,
            entries: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PersistDurability {
    Durable,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PromotionKind {
    Inserted,
    Refreshed,
    Reactivated,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RegistryCounts {
    pub(crate) active: usize,
    pub(crate) disabled: usize,
    pub(crate) revoked: usize,
}

pub(super) struct TeamsConversationRegistry {
    path: PathBuf,
    state: TeamsConversationRegistryFile,
    max_entries: usize,
    ttl_secs: i64,
}

impl TeamsConversationRegistry {
    pub(super) fn open(raw_path: &str, max_entries: usize, ttl_secs: u64) -> Result<Self> {
        if !(1..=REGISTRY_MAX_CONFIGURED_ENTRIES).contains(&max_entries) {
            bail!(
                "conversation registry max entries must be between 1 and {}",
                REGISTRY_MAX_CONFIGURED_ENTRIES
            );
        }
        let ttl_secs = i64::try_from(ttl_secs)
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| anyhow!("conversation registry TTL is out of range"))?;
        let path = resolve_registry_path(raw_path)?;
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("conversation registry path has no parent"))?;
        ensure_safe_directory(parent)?;
        cleanup_registry_temps(&path)?;

        let mut state = if path.exists() {
            load_registry_file(&path, REGISTRY_MAX_CONFIGURED_ENTRIES)?
        } else {
            TeamsConversationRegistryFile::default()
        };
        let now = Utc::now();
        let before = state.entries.len();
        prune_expired(&mut state.entries, now, ttl_secs);
        trim_to_capacity(&mut state.entries, max_entries)?;
        if state.entries.len() != before {
            state.generation = next_generation(state.generation)?;
            if persist_registry_file(&path, &state)? == PersistDurability::Unknown {
                bail!("conversation registry startup cleanup durability is unknown");
            }
        }

        Ok(Self {
            path,
            state,
            max_entries,
            ttl_secs,
        })
    }

    pub(super) fn promote(
        &mut self,
        route: &TeamsIngressRoute,
        now: DateTime<Utc>,
    ) -> Result<PromotionKind> {
        let entry = entry_from_route(route, now)?;
        let key = entry.key.clone();
        let mut candidate = self.state.clone();
        prune_expired(&mut candidate.entries, now, self.ttl_secs);

        let promotion = if let Some(existing) = candidate
            .entries
            .iter_mut()
            .find(|existing| existing.key == key)
        {
            let kind = if existing.state == TeamsConversationState::Active {
                PromotionKind::Refreshed
            } else {
                PromotionKind::Reactivated
            };
            let mut refreshed = entry;
            if refreshed.conversation_type == existing.conversation_type {
                if refreshed.team_id.is_none() {
                    refreshed.team_id.clone_from(&existing.team_id);
                }
                if refreshed.channel_id.is_none() {
                    refreshed.channel_id.clone_from(&existing.channel_id);
                }
            }
            *existing = refreshed;
            kind
        } else {
            make_capacity(&mut candidate.entries, self.max_entries)?;
            candidate.entries.push(entry);
            PromotionKind::Inserted
        };
        commit_candidate(&self.path, &mut self.state, candidate)?;
        Ok(promotion)
    }

    pub(super) fn revoke(
        &mut self,
        key: &TeamsConversationKey,
        reason_code: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        Ok(self.revoke_scope(key, None, reason_code, now)? > 0)
    }

    pub(super) fn revoke_scope(
        &mut self,
        key: &TeamsConversationKey,
        team_id: Option<&str>,
        reason_code: &str,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        validate_key(key)?;
        validate_bounded_field("reason code", reason_code, FIELD_LIMIT)?;
        validate_optional_id("team id", team_id)?;
        let mut candidate = self.state.clone();
        let mut changed = 0;
        for entry in &mut candidate.entries {
            let identity_matches = entry.key.app_id == key.app_id
                && entry.key.tenant_id == key.tenant_id
                && entry.key.bot_framework_channel_id == key.bot_framework_channel_id;
            let scope_matches = match team_id {
                Some(team_id) => entry.team_id.as_deref() == Some(team_id),
                None => entry.key == *key,
            };
            if !identity_matches
                || !scope_matches
                || (entry.state == TeamsConversationState::Revoked
                    && entry.reason_code.as_deref() == Some(reason_code))
            {
                continue;
            }
            entry.state = TeamsConversationState::Revoked;
            entry.reason_code = Some(reason_code.into());
            entry.consecutive_forbidden_writes = 0;
            entry.updated_at = now;
            changed += 1;
        }
        if changed == 0 {
            return Ok(0);
        }
        commit_candidate(&self.path, &mut self.state, candidate)?;
        Ok(changed)
    }

    /// PR 12 feeds explicit blocked/not-in-roster outcomes into this transition.
    #[allow(dead_code)]
    pub(super) fn record_forbidden_write(
        &mut self,
        key: &TeamsConversationKey,
        reason_code: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        validate_bounded_field("reason code", reason_code, FIELD_LIMIT)?;
        let mut candidate = self.state.clone();
        let Some(entry) = candidate.entries.iter_mut().find(|entry| &entry.key == key) else {
            return Ok(false);
        };
        if entry.state == TeamsConversationState::Revoked {
            return Ok(false);
        }
        entry.consecutive_forbidden_writes = entry
            .consecutive_forbidden_writes
            .saturating_add(1)
            .min(FORBIDDEN_DISABLE_THRESHOLD);
        if entry.consecutive_forbidden_writes >= FORBIDDEN_DISABLE_THRESHOLD {
            entry.state = TeamsConversationState::Disabled;
            entry.reason_code = Some(reason_code.into());
        }
        entry.updated_at = now;
        commit_candidate(&self.path, &mut self.state, candidate)?;
        Ok(true)
    }

    /// PR 12 clears a prior single forbidden result after a confirmed delivery.
    #[allow(dead_code)]
    pub(super) fn record_success(
        &mut self,
        key: &TeamsConversationKey,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let mut candidate = self.state.clone();
        let Some(entry) = candidate.entries.iter_mut().find(|entry| &entry.key == key) else {
            return Ok(false);
        };
        if entry.state != TeamsConversationState::Active || entry.consecutive_forbidden_writes == 0
        {
            return Ok(false);
        }
        entry.consecutive_forbidden_writes = 0;
        entry.reason_code = None;
        entry.updated_at = now;
        commit_candidate(&self.path, &mut self.state, candidate)?;
        Ok(true)
    }

    /// PR 12 consumes only an active, non-expired copy.
    #[allow(dead_code)]
    pub(super) fn active(
        &self,
        key: &TeamsConversationKey,
        now: DateTime<Utc>,
    ) -> Option<TeamsConversationEntry> {
        self.state
            .entries
            .iter()
            .find(|entry| {
                &entry.key == key
                    && entry.state == TeamsConversationState::Active
                    && !is_expired(entry, now, self.ttl_secs)
            })
            .cloned()
    }

    pub(super) fn counts(&self) -> RegistryCounts {
        let mut counts = RegistryCounts::default();
        for entry in &self.state.entries {
            match entry.state {
                TeamsConversationState::Active => counts.active += 1,
                TeamsConversationState::Disabled => counts.disabled += 1,
                TeamsConversationState::Revoked => counts.revoked += 1,
            }
        }
        counts
    }

    #[cfg(test)]
    fn generation(&self) -> u64 {
        self.state.generation
    }

    #[cfg(test)]
    pub(super) fn insert_route_unchecked_for_test(
        &mut self,
        route: &TeamsIngressRoute,
        now: DateTime<Utc>,
    ) {
        let key = key_for_route(route);
        self.state.entries.retain(|entry| entry.key != key);
        self.state.entries.push(TeamsConversationEntry {
            schema_version: REGISTRY_VERSION,
            key,
            conversation_type: route.conversation_type.clone(),
            service_url: route.service_url.as_str().into(),
            team_id: route.team_id.clone(),
            channel_id: route.channel_id.clone(),
            last_validated_at: now,
            updated_at: now,
            state: TeamsConversationState::Active,
            reason_code: None,
            consecutive_forbidden_writes: 0,
        });
    }

    #[cfg(test)]
    pub(super) fn entry_for_test(
        &self,
        key: &TeamsConversationKey,
    ) -> Option<TeamsConversationEntry> {
        self.state
            .entries
            .iter()
            .find(|entry| &entry.key == key)
            .cloned()
    }
}

pub(super) fn key_for_route(route: &TeamsIngressRoute) -> TeamsConversationKey {
    TeamsConversationKey {
        app_id: route.key.app_id.clone(),
        tenant_id: route.tenant_id.clone(),
        bot_framework_channel_id: route.bot_framework_channel_id.clone(),
        conversation_id: route.conversation_id.clone(),
    }
}

pub(super) fn key_from_parts(
    app_id: &str,
    tenant_id: &str,
    bot_framework_channel_id: &str,
    conversation_id: &str,
) -> Result<TeamsConversationKey> {
    let key = TeamsConversationKey {
        app_id: app_id.into(),
        tenant_id: tenant_id.into(),
        bot_framework_channel_id: bot_framework_channel_id.into(),
        conversation_id: conversation_id.into(),
    };
    validate_key(&key)?;
    Ok(key)
}

fn entry_from_route(
    route: &TeamsIngressRoute,
    now: DateTime<Utc>,
) -> Result<TeamsConversationEntry> {
    let key = key_for_route(route);
    validate_key(&key)?;
    validate_bounded_field("conversation type", &route.conversation_type, FIELD_LIMIT)?;
    if !matches!(
        route.conversation_type.as_str(),
        "personal" | "groupChat" | "channel"
    ) {
        bail!("conversation registry requires a canonical conversation type");
    }
    validate_optional_id("team id", route.team_id.as_deref())?;
    validate_optional_id("channel id", route.channel_id.as_deref())?;
    if route.conversation_type == "channel"
        && (route.team_id.is_none() || route.channel_id.is_none())
    {
        bail!("channel conversation registry route is missing Team identity");
    }
    let service_url = route.service_url.as_str();
    validate_service_url(service_url)?;

    Ok(TeamsConversationEntry {
        schema_version: REGISTRY_VERSION,
        key,
        conversation_type: route.conversation_type.clone(),
        service_url: service_url.into(),
        team_id: route.team_id.clone(),
        channel_id: route.channel_id.clone(),
        last_validated_at: now,
        updated_at: now,
        state: TeamsConversationState::Active,
        reason_code: None,
        consecutive_forbidden_writes: 0,
    })
}

fn validate_registry_file(state: &TeamsConversationRegistryFile, max_entries: usize) -> Result<()> {
    if state.schema != REGISTRY_SCHEMA || state.version != REGISTRY_VERSION {
        bail!("conversation registry schema version is unsupported");
    }
    if state.entries.len() > max_entries {
        bail!("conversation registry exceeds configured entry capacity");
    }
    let mut keys = HashSet::with_capacity(state.entries.len());
    for entry in &state.entries {
        if entry.schema_version != REGISTRY_VERSION {
            bail!("conversation registry entry schema version is unsupported");
        }
        validate_key(&entry.key)?;
        validate_bounded_field("conversation type", &entry.conversation_type, FIELD_LIMIT)?;
        if !matches!(
            entry.conversation_type.as_str(),
            "personal" | "groupChat" | "channel"
        ) {
            bail!("conversation registry contains an unknown conversation type");
        }
        validate_service_url(&entry.service_url)?;
        validate_optional_id("team id", entry.team_id.as_deref())?;
        validate_optional_id("channel id", entry.channel_id.as_deref())?;
        if entry.conversation_type == "channel"
            && (entry.team_id.is_none() || entry.channel_id.is_none())
        {
            bail!("conversation registry channel entry is missing Team identity");
        }
        if let Some(reason) = entry.reason_code.as_deref() {
            validate_bounded_field("reason code", reason, FIELD_LIMIT)?;
        }
        if entry.consecutive_forbidden_writes > FORBIDDEN_DISABLE_THRESHOLD {
            bail!("conversation registry contains an invalid failure count");
        }
        if !keys.insert(entry.key.clone()) {
            bail!("conversation registry contains a duplicate composite key");
        }
    }
    Ok(())
}

fn validate_key(key: &TeamsConversationKey) -> Result<()> {
    validate_bounded_field("app id", &key.app_id, FIELD_LIMIT)?;
    validate_bounded_field("tenant id", &key.tenant_id, FIELD_LIMIT)?;
    validate_bounded_field(
        "Bot Framework channel id",
        &key.bot_framework_channel_id,
        FIELD_LIMIT,
    )?;
    validate_bounded_field("conversation id", &key.conversation_id, ROUTE_ID_LIMIT)
}

fn validate_optional_id(label: &str, value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        validate_bounded_field(label, value, ROUTE_ID_LIMIT)?;
    }
    Ok(())
}

fn validate_bounded_field(label: &str, value: &str, max_bytes: usize) -> Result<()> {
    if value.trim().is_empty()
        || value.trim() != value
        || value.len() > max_bytes
        || value.chars().any(char::is_control)
    {
        bail!("conversation registry {label} is invalid");
    }
    Ok(())
}

fn validate_service_url(raw: &str) -> Result<()> {
    validate_bounded_field("service URL", raw, SERVICE_URL_LIMIT)?;
    let url = reqwest::Url::parse(raw).context("conversation registry service URL is malformed")?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port().is_some()
        || !url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("smba.trafficmanager.net"))
    {
        bail!("conversation registry service URL is outside the public Teams boundary");
    }
    Ok(())
}

fn eviction_candidate(entries: &[TeamsConversationEntry]) -> Result<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.state != TeamsConversationState::Revoked)
        .min_by_key(|(_, entry)| {
            let state_order = match entry.state {
                TeamsConversationState::Disabled => 0,
                TeamsConversationState::Active => 1,
                TeamsConversationState::Revoked => 2,
            };
            (
                state_order,
                entry.last_validated_at,
                entry.key.conversation_id.as_str(),
            )
        })
        .map(|(index, _)| index)
        .ok_or_else(|| anyhow!("conversation registry is saturated by revoked records"))
}

fn make_capacity(entries: &mut Vec<TeamsConversationEntry>, max_entries: usize) -> Result<()> {
    while entries.len() >= max_entries {
        let candidate = eviction_candidate(entries)?;
        entries.remove(candidate);
    }
    Ok(())
}

fn trim_to_capacity(entries: &mut Vec<TeamsConversationEntry>, max_entries: usize) -> Result<()> {
    while entries.len() > max_entries {
        let candidate = eviction_candidate(entries)?;
        entries.remove(candidate);
    }
    Ok(())
}

fn prune_expired(entries: &mut Vec<TeamsConversationEntry>, now: DateTime<Utc>, ttl_secs: i64) {
    entries.retain(|entry| {
        entry.state == TeamsConversationState::Revoked || !is_expired(entry, now, ttl_secs)
    });
}

fn is_expired(entry: &TeamsConversationEntry, now: DateTime<Utc>, ttl_secs: i64) -> bool {
    now.signed_duration_since(entry.last_validated_at)
        .num_seconds()
        > ttl_secs
}

fn commit_candidate(
    path: &Path,
    current: &mut TeamsConversationRegistryFile,
    mut candidate: TeamsConversationRegistryFile,
) -> Result<()> {
    candidate.generation = next_generation(current.generation)?;
    candidate.entries.sort_by(|left, right| {
        (
            left.key.app_id.as_str(),
            left.key.tenant_id.as_str(),
            left.key.bot_framework_channel_id.as_str(),
            left.key.conversation_id.as_str(),
        )
            .cmp(&(
                right.key.app_id.as_str(),
                right.key.tenant_id.as_str(),
                right.key.bot_framework_channel_id.as_str(),
                right.key.conversation_id.as_str(),
            ))
    });
    let durability = persist_registry_file(path, &candidate)?;
    *current = candidate;
    if durability == PersistDurability::Unknown {
        bail!("conversation registry commit durability is unknown");
    }
    Ok(())
}

fn next_generation(current: u64) -> Result<u64> {
    current
        .checked_add(1)
        .ok_or_else(|| anyhow!("conversation registry generation overflow"))
}

fn resolve_registry_path(raw: &str) -> Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() || raw.as_bytes().contains(&0) {
        bail!("conversation registry path is empty or invalid");
    }
    let configured = PathBuf::from(raw);
    reject_unsafe_components(&configured)?;
    let resolved = if configured.is_absolute() {
        configured
    } else {
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .or_else(|| std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()))
            .ok_or_else(|| {
                anyhow!("HOME or USERPROFILE is required for a relative conversation registry path")
            })?;
        PathBuf::from(home).join(".openab").join(configured)
    };
    if !resolved.is_absolute() {
        bail!("conversation registry path did not resolve to an absolute path");
    }
    reject_unsafe_components(&resolved)?;
    Ok(resolved)
}

fn reject_unsafe_components(path: &Path) -> Result<()> {
    for component in path.components() {
        if matches!(component, Component::CurDir | Component::ParentDir) {
            bail!("conversation registry path contains traversal components");
        }
        if matches!(component, Component::Normal(value) if value.is_empty()) {
            bail!("conversation registry path contains an empty component");
        }
    }
    Ok(())
}

fn ensure_safe_directory(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    bail!("conversation registry parent path is not a safe directory");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_registry_directory(&current)?;
                set_directory_permissions(&current)?;
                let metadata = fs::symlink_metadata(&current)
                    .context("failed to verify conversation registry directory")?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    bail!("conversation registry directory verification failed");
                }
            }
            Err(error) => {
                return Err(error).context("failed to inspect conversation registry directory")
            }
        }
    }
    Ok(())
}

fn load_registry_file(path: &Path, max_entries: usize) -> Result<TeamsConversationRegistryFile> {
    let metadata = fs::symlink_metadata(path).context("failed to inspect conversation registry")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("conversation registry path is not a regular file");
    }
    if metadata.len() > REGISTRY_FILE_MAX_BYTES {
        bail!("conversation registry file exceeds the byte limit");
    }
    set_file_permissions(path)?;
    let file = open_registry_read(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(REGISTRY_FILE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("failed to read conversation registry")?;
    if bytes.len() as u64 > REGISTRY_FILE_MAX_BYTES {
        bail!("conversation registry file exceeds the byte limit");
    }
    let state: TeamsConversationRegistryFile =
        serde_json::from_slice(&bytes).context("conversation registry JSON is invalid")?;
    validate_registry_file(&state, max_entries)?;
    Ok(state)
}

fn persist_registry_file(
    path: &Path,
    state: &TeamsConversationRegistryFile,
) -> Result<PersistDurability> {
    validate_registry_file(state, REGISTRY_MAX_CONFIGURED_ENTRIES)?;
    let mut bytes =
        serde_json::to_vec(state).context("failed to serialize conversation registry")?;
    bytes.push(b'\n');
    if bytes.len() as u64 > REGISTRY_FILE_MAX_BYTES {
        bail!("conversation registry candidate exceeds the byte limit");
    }

    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("conversation registry path has no parent"))?;
    ensure_safe_directory(parent)?;
    reject_unsafe_target(path)?;
    let temp = registry_temp_path(path);
    let result = (|| -> Result<PersistDurability> {
        let mut file = open_registry_temp(&temp)?;
        set_file_permissions(&temp)?;
        file.write_all(&bytes)
            .context("failed to write conversation registry temporary file")?;
        file.flush()
            .context("failed to flush conversation registry temporary file")?;
        file.sync_all()
            .context("failed to sync conversation registry temporary file")?;
        drop(file);
        reject_unsafe_target(path)?;
        atomic_replace(&temp, path).context("failed to replace conversation registry")?;
        Ok(match sync_parent(parent) {
            Ok(()) => PersistDurability::Durable,
            Err(_) => PersistDurability::Unknown,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn reject_unsafe_target(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!("conversation registry target is not a safe regular file")
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("failed to inspect conversation registry target"),
    }
}

fn registry_temp_path(path: &Path) -> PathBuf {
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("registry");
    path.with_file_name(format!(
        ".{filename}{REGISTRY_TEMP_MARKER}{}",
        uuid::Uuid::new_v4()
    ))
}

fn cleanup_registry_temps(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let Some(filename) = path.file_name().and_then(|value| value.to_str()) else {
        bail!("conversation registry filename is not valid UTF-8");
    };
    let prefix = format!(".{filename}{REGISTRY_TEMP_MARKER}");
    for entry in
        fs::read_dir(parent).context("failed to inspect conversation registry directory")?
    {
        let entry = entry.context("failed to inspect conversation registry temporary file")?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        if uuid::Uuid::parse_str(suffix).is_err() {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())
            .context("failed to verify conversation registry temporary file")?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("conversation registry temporary path is unsafe");
        }
        fs::remove_file(entry.path())
            .context("failed to remove stale conversation registry temporary file")?;
    }
    Ok(())
}

fn open_registry_read(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .context("failed to open conversation registry")
}

fn open_registry_temp(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .context("failed to create conversation registry temporary file")
}

#[cfg(unix)]
fn create_registry_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(path)
        .context("failed to create conversation registry directory")
}

#[cfg(not(unix))]
fn create_registry_directory(path: &Path) -> Result<()> {
    fs::create_dir(path).context("failed to create conversation registry directory")
}

#[cfg(unix)]
fn set_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .context("failed to secure conversation registry directory")
}

#[cfg(not(unix))]
fn set_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .context("failed to secure conversation registry file")
}

#[cfg(not(unix))]
fn set_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn atomic_replace(temp: &Path, target: &Path) -> std::io::Result<()> {
    fs::rename(temp, target)
}

#[cfg(windows)]
fn atomic_replace(temp: &Path, target: &Path) -> std::io::Result<()> {
    if !target.exists() {
        return fs::rename(temp, target);
    }
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

    let target_wide: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let temp_wide: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: both UTF-16 buffers are NUL-terminated, remain alive for the
    // synchronous call, and ReplaceFileW does not retain supplied pointers.
    let result = unsafe {
        ReplaceFileW(
            target_wide.as_ptr(),
            temp_wide.as_ptr(),
            ptr::null(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<()> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .context("failed to sync conversation registry directory")
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::teams_ingress::{TeamsIngressRoute, TeamsRouteKey};
    use reqwest::Url;
    use std::collections::HashMap;
    use std::time::Instant;

    fn test_dir(label: &str) -> PathBuf {
        let root = fs::canonicalize(std::env::temp_dir()).expect("temp root");
        let path = root.join(format!(
            "openab-teams-registry-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&path).expect("test directory");
        path
    }

    fn route(conversation_id: &str) -> TeamsIngressRoute {
        let key = TeamsRouteKey::new("app", "tenant", conversation_id, "activity-secret");
        TeamsIngressRoute {
            key,
            event_id: "event-secret".into(),
            tenant_id: "tenant".into(),
            bot_framework_channel_id: "msteams".into(),
            conversation_id: conversation_id.into(),
            conversation_type: "personal".into(),
            inbound_activity_id: "activity-secret".into(),
            reply_chain_root_id: None,
            service_url: Url::parse("https://smba.trafficmanager.net/teams")
                .expect("test setup or assertion invariant"),
            team_id: None,
            channel_id: None,
            attachment_sources: HashMap::new(),
            attachment_materialized_bytes: 0,
            created_at: Instant::now(),
        }
    }

    fn open(path: &Path, max_entries: usize) -> TeamsConversationRegistry {
        TeamsConversationRegistry::open(
            path.to_str().expect("test setup or assertion invariant"),
            max_entries,
            3600,
        )
        .expect("test setup or assertion invariant")
    }

    #[test]
    fn trusted_route_round_trips_without_activity_or_event_identifiers() {
        let dir = test_dir("roundtrip");
        let path = dir.join("registry.json");
        let now = Utc::now();
        let mut registry = open(&path, 10);
        assert_eq!(
            registry.promote(&route("conversation"), now).unwrap(),
            PromotionKind::Inserted
        );
        assert_eq!(registry.counts().active, 1);
        assert_eq!(registry.generation(), 1);

        let raw = fs::read_to_string(&path).expect("test setup or assertion invariant");
        assert!(!raw.contains("activity-secret"));
        assert!(!raw.contains("event-secret"));
        assert!(raw.contains("smba.trafficmanager.net"));

        let reopened = open(&path, 10);
        let key = key_for_route(&route("conversation"));
        assert!(reopened.active(&key, now).is_some());
        assert_eq!(reopened.generation(), 1);
        fs::remove_dir_all(dir).expect("test setup or assertion invariant");
    }

    #[test]
    fn complete_composite_identity_prevents_cross_scope_collision() {
        let dir = test_dir("composite-key");
        let path = dir.join("registry.json");
        let now = Utc::now();
        let mut registry = open(&path, 10);
        let first = route("shared-conversation");
        let mut second = route("shared-conversation");
        second.key = TeamsRouteKey::new(
            "other-app",
            "other-tenant",
            "shared-conversation",
            "other-activity",
        );
        second.tenant_id = "other-tenant".into();
        second.bot_framework_channel_id = "other-channel".into();
        registry
            .promote(&first, now)
            .expect("test setup or assertion invariant");
        registry
            .promote(&second, now)
            .expect("test setup or assertion invariant");
        assert_eq!(registry.counts().active, 2);
        assert!(registry.active(&key_for_route(&first), now).is_some());
        assert!(registry.active(&key_for_route(&second), now).is_some());
        fs::remove_dir_all(dir).expect("test setup or assertion invariant");
    }

    #[test]
    fn sparse_refresh_preserves_optional_channel_identity() {
        let dir = test_dir("sparse-refresh");
        let path = dir.join("registry.json");
        let now = Utc::now();
        let mut registry = open(&path, 10);
        let mut first = route("conversation");
        first.conversation_type = "groupChat".into();
        first.team_id = Some("team".into());
        first.channel_id = Some("channel".into());
        registry
            .promote(&first, now)
            .expect("test setup or assertion invariant");

        let mut sparse = first.clone();
        sparse.team_id = None;
        sparse.channel_id = None;
        assert_eq!(
            registry.promote(&sparse, now).unwrap(),
            PromotionKind::Refreshed
        );
        let entry = registry
            .active(&key_for_route(&sparse), now)
            .expect("test setup or assertion invariant");
        assert_eq!(entry.team_id.as_deref(), Some("team"));
        assert_eq!(entry.channel_id.as_deref(), Some("channel"));
        fs::remove_dir_all(dir).expect("test setup or assertion invariant");
    }

    #[test]
    fn team_installation_removal_revokes_every_record_in_that_team_only() {
        let dir = test_dir("team-revoke");
        let path = dir.join("registry.json");
        let now = Utc::now();
        let mut registry = open(&path, 10);
        let mut first = route("team-conversation-1");
        first.conversation_type = "channel".into();
        first.team_id = Some("team-1".into());
        first.channel_id = Some("channel-1".into());
        let mut second = route("team-conversation-2");
        second.conversation_type = "channel".into();
        second.team_id = Some("team-1".into());
        second.channel_id = Some("channel-2".into());
        let mut other = route("team-conversation-3");
        other.conversation_type = "channel".into();
        other.team_id = Some("team-2".into());
        other.channel_id = Some("channel-3".into());
        for route in [&first, &second, &other] {
            registry
                .promote(route, now)
                .expect("test setup or assertion invariant");
        }
        assert_eq!(
            registry
                .revoke_scope(
                    &key_for_route(&first),
                    Some("team-1"),
                    "installation_remove",
                    now,
                )
                .expect("test setup or assertion invariant"),
            2
        );
        assert_eq!(registry.counts().revoked, 2);
        assert_eq!(registry.counts().active, 1);
        assert!(registry.active(&key_for_route(&other), now).is_some());
        fs::remove_dir_all(dir).expect("test setup or assertion invariant");
    }

    #[test]
    fn refresh_reactivates_disabled_and_revoked_records() {
        let dir = test_dir("transitions");
        let path = dir.join("registry.json");
        let mut registry = open(&path, 10);
        let route = route("conversation");
        let key = key_for_route(&route);
        let now = Utc::now();
        registry
            .promote(&route, now)
            .expect("test setup or assertion invariant");
        registry
            .record_forbidden_write(&key, "message_writes_blocked", now)
            .expect("test setup or assertion invariant");
        registry
            .record_forbidden_write(&key, "message_writes_blocked", now)
            .expect("test setup or assertion invariant");
        assert_eq!(registry.counts().disabled, 1);
        assert_eq!(
            registry.promote(&route, now).unwrap(),
            PromotionKind::Reactivated
        );
        assert_eq!(registry.counts().active, 1);
        assert!(registry.revoke(&key, "installation_remove", now).unwrap());
        assert_eq!(registry.counts().revoked, 1);
        assert_eq!(
            registry.promote(&route, now).unwrap(),
            PromotionKind::Reactivated
        );
        assert_eq!(registry.counts().active, 1);
        fs::remove_dir_all(dir).expect("test setup or assertion invariant");
    }

    #[test]
    fn successful_write_clears_one_forbidden_result_without_reactivating() {
        let dir = test_dir("success-reset");
        let path = dir.join("registry.json");
        let mut registry = open(&path, 10);
        let route = route("conversation");
        let key = key_for_route(&route);
        let now = Utc::now();
        registry
            .promote(&route, now)
            .expect("test setup or assertion invariant");
        registry
            .record_forbidden_write(&key, "message_writes_blocked", now)
            .expect("test setup or assertion invariant");
        assert!(registry.record_success(&key, now).unwrap());
        let entry = registry
            .active(&key, now)
            .expect("test setup or assertion invariant");
        assert_eq!(entry.consecutive_forbidden_writes, 0);
        fs::remove_dir_all(dir).expect("test setup or assertion invariant");
    }

    #[test]
    fn lowering_capacity_trims_disabled_before_active_on_restart() {
        let dir = test_dir("lower-capacity");
        let path = dir.join("registry.json");
        let now = Utc::now();
        let mut registry = open(&path, 2);
        let disabled = route("disabled");
        let disabled_key = key_for_route(&disabled);
        registry
            .promote(&disabled, now)
            .expect("test setup or assertion invariant");
        registry
            .record_forbidden_write(&disabled_key, "blocked", now)
            .expect("test setup or assertion invariant");
        registry
            .record_forbidden_write(&disabled_key, "blocked", now)
            .expect("test setup or assertion invariant");
        registry
            .promote(&route("active"), now)
            .expect("test setup or assertion invariant");
        drop(registry);

        let reopened = open(&path, 1);
        assert_eq!(reopened.counts().active, 1);
        assert_eq!(reopened.counts().disabled, 0);
        fs::remove_dir_all(dir).expect("test setup or assertion invariant");
    }

    #[test]
    fn revoked_records_saturate_instead_of_being_evicted() {
        let dir = test_dir("revoked-capacity");
        let path = dir.join("registry.json");
        let mut registry = open(&path, 1);
        let first = route("first");
        let key = key_for_route(&first);
        let now = Utc::now();
        registry
            .promote(&first, now)
            .expect("test setup or assertion invariant");
        registry
            .revoke(&key, "installation_remove", now)
            .expect("test setup or assertion invariant");
        assert!(registry.promote(&route("second"), now).is_err());
        assert_eq!(registry.counts().revoked, 1);
        fs::remove_dir_all(dir).expect("test setup or assertion invariant");
    }

    #[test]
    fn disabled_record_is_evicted_before_active_record() {
        let dir = test_dir("capacity-order");
        let path = dir.join("registry.json");
        let mut registry = open(&path, 2);
        let now = Utc::now();
        let disabled = route("disabled");
        let disabled_key = key_for_route(&disabled);
        registry
            .promote(&disabled, now)
            .expect("test setup or assertion invariant");
        registry
            .record_forbidden_write(&disabled_key, "blocked", now)
            .expect("test setup or assertion invariant");
        registry
            .record_forbidden_write(&disabled_key, "blocked", now)
            .expect("test setup or assertion invariant");
        registry
            .promote(&route("active"), now)
            .expect("test setup or assertion invariant");
        registry
            .promote(&route("new"), now)
            .expect("test setup or assertion invariant");
        assert_eq!(registry.counts().disabled, 0);
        assert_eq!(registry.counts().active, 2);
        fs::remove_dir_all(dir).expect("test setup or assertion invariant");
    }

    #[test]
    fn expired_active_records_are_pruned_on_restart() {
        let dir = test_dir("ttl");
        let path = dir.join("registry.json");
        let old = Utc::now() - chrono::Duration::seconds(5);
        let mut registry = TeamsConversationRegistry::open(
            path.to_str().expect("test setup or assertion invariant"),
            10,
            1,
        )
        .expect("test setup or assertion invariant");
        registry
            .promote(&route("conversation"), old)
            .expect("test setup or assertion invariant");
        drop(registry);

        let reopened = TeamsConversationRegistry::open(
            path.to_str().expect("test setup or assertion invariant"),
            10,
            1,
        )
        .expect("test setup or assertion invariant");
        assert_eq!(reopened.counts(), RegistryCounts::default());
        assert_eq!(reopened.generation(), 2);
        fs::remove_dir_all(dir).expect("test setup or assertion invariant");
    }

    #[test]
    fn failed_candidate_does_not_replace_the_committed_generation() {
        let dir = test_dir("failed-candidate");
        let path = dir.join("registry.json");
        let mut registry = open(&path, 10);
        registry
            .promote(&route("conversation"), Utc::now())
            .expect("test setup or assertion invariant");
        let before = fs::read(&path).expect("test setup or assertion invariant");
        let mut unsafe_route = route("other");
        unsafe_route.service_url =
            Url::parse("https://example.com/teams").expect("test setup or assertion invariant");
        assert!(registry.promote(&unsafe_route, Utc::now()).is_err());
        let oversized_conversation = "x".repeat(ROUTE_ID_LIMIT + 1);
        assert!(registry
            .promote(&route(&oversized_conversation), Utc::now())
            .is_err());
        assert_eq!(
            fs::read(&path).expect("test setup or assertion invariant"),
            before
        );
        assert_eq!(registry.generation(), 1);
        fs::remove_dir_all(dir).expect("test setup or assertion invariant");
    }

    #[test]
    fn corrupt_or_unknown_file_is_not_replaced() {
        let dir = test_dir("corrupt");
        let path = dir.join("registry.json");
        fs::write(&path, b"{not-json").expect("test setup or assertion invariant");
        let before = fs::read(&path).expect("test setup or assertion invariant");
        assert!(TeamsConversationRegistry::open(path.to_str().unwrap(), 10, 3600).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);

        let unknown = br#"{"schema":"openab.teams.conversation_registry.v2","version":2,"generation":0,"entries":[]}"#;
        fs::write(&path, unknown).expect("test setup or assertion invariant");
        assert!(TeamsConversationRegistry::open(path.to_str().unwrap(), 10, 3600).is_err());
        assert_eq!(fs::read(&path).unwrap(), unknown);

        let oversized = File::create(&path).expect("test setup or assertion invariant");
        oversized
            .set_len(REGISTRY_FILE_MAX_BYTES + 1)
            .expect("test setup or assertion invariant");
        drop(oversized);
        assert!(TeamsConversationRegistry::open(path.to_str().unwrap(), 10, 3600).is_err());
        assert_eq!(
            fs::metadata(&path).unwrap().len(),
            REGISTRY_FILE_MAX_BYTES + 1
        );
        assert!(TeamsConversationRegistry::open("../traversal.json", 10, 3600).is_err());
        fs::remove_dir_all(dir).expect("test setup or assertion invariant");
    }

    #[cfg(unix)]
    #[test]
    fn file_permissions_are_tightened_and_symlink_targets_are_rejected() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = test_dir("permissions");
        let path = dir.join("registry.json");
        let mut registry = open(&path, 10);
        registry
            .promote(&route("conversation"), Utc::now())
            .expect("test setup or assertion invariant");
        assert_eq!(
            fs::metadata(&path)
                .expect("test setup or assertion invariant")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let nested = dir.join("created").join("nested").join("registry.json");
        let _nested_registry = TeamsConversationRegistry::open(
            nested.to_str().expect("test setup or assertion invariant"),
            10,
            3600,
        )
        .expect("test setup or assertion invariant");
        for created in [dir.join("created"), dir.join("created").join("nested")] {
            assert_eq!(
                fs::metadata(created)
                    .expect("test setup or assertion invariant")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }

        let target = dir.join("target.json");
        fs::write(&target, b"{}").expect("test setup or assertion invariant");
        let link = dir.join("link.json");
        symlink(&target, &link).expect("test setup or assertion invariant");
        assert!(TeamsConversationRegistry::open(
            link.to_str().expect("test setup or assertion invariant"),
            10,
            3600
        )
        .is_err());

        let real_parent = dir.join("real-parent");
        fs::create_dir(&real_parent).expect("test setup or assertion invariant");
        let linked_parent = dir.join("linked-parent");
        symlink(&real_parent, &linked_parent).expect("test setup or assertion invariant");
        let linked_path = linked_parent.join("registry.json");
        assert!(TeamsConversationRegistry::open(
            linked_path
                .to_str()
                .expect("test setup or assertion invariant"),
            10,
            3600
        )
        .is_err());
        fs::remove_dir_all(dir).expect("test setup or assertion invariant");
    }

    #[test]
    fn only_valid_registry_temp_names_are_removed() {
        let dir = test_dir("temp-cleanup");
        let path = dir.join("registry.json");
        let stale = dir.join(format!(
            ".registry.json{REGISTRY_TEMP_MARKER}{}",
            uuid::Uuid::new_v4()
        ));
        let unrelated = dir.join(".registry.json.tmp-keep-me");
        fs::write(&stale, b"stale").expect("test setup or assertion invariant");
        fs::write(&unrelated, b"unrelated").expect("test setup or assertion invariant");
        let _registry = open(&path, 10);
        assert!(!stale.exists());
        assert!(unrelated.exists());
        fs::remove_dir_all(dir).expect("test setup or assertion invariant");
    }
}

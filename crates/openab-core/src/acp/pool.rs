use crate::acp::connection::{AcpConnection, SessionActivity};
use crate::acp::project::ProjectContext;
use crate::acp::protocol::ConfigOption;
use crate::config::AgentConfig;
use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio::time::Instant;
use tracing::{info, warn};

/// Error substrings produced by `AcpConnection::send_request` that indicate a
/// transient failure worth preserving the session ID for retry, as opposed to
/// a permanent agent-side rejection.
const TRANSIENT_LOAD_ERRORS: &[&str] = &["timeout waiting for", "channel closed"];

/// Combined state protected by a single lock to prevent deadlocks.
/// Lock ordering: never await a per-connection mutex while holding `state`.
struct PoolState {
    /// Active connections: thread_key → AcpConnection handle.
    active: HashMap<String, Arc<Mutex<AcpConnection>>>,
    /// Lock-free cancel handles: thread_key → (stdin, session_id).
    /// Stored separately so cancel can work without locking the connection.
    cancel_handles: HashMap<String, CancelHandle>,
    /// Lock-free facade tokens: thread_key → the exact `OPENAB_SESSION_TOKEN` minted for the
    /// connection currently under that key. Stored here, not just inside the connection, so hung
    /// eviction can revoke the exact token **synchronously** — the `AcpConnection` DropGuard that
    /// normally revokes it cannot fire while a hung streaming task still holds an Arc of the
    /// connection, and `AcpTunnelSource` authorizes by channel alone, so an un-revoked predecessor
    /// token would keep reaching whatever tunnel a successor registers for that channel (F3).
    #[cfg(feature = "acp-mcp")]
    facade_tokens: HashMap<String, String>,
    /// Lock-free activity handles for hung-session detection without the connection mutex.
    activity: HashMap<String, Arc<SessionActivity>>,
    /// Child process-group ids, captured at insert time so hung eviction can
    /// kill the agent process without ever locking the connection.
    pgids: HashMap<String, i32>,
    /// Suspended sessions: thread_key → ACP sessionId.
    /// Used at runtime to decide which thread can be resumed via `session/load`
    /// because it no longer has a live in-memory connection.
    suspended: HashMap<String, String>,
    /// Persisted resumable sessions: thread_key → ACP sessionId.
    /// Includes both suspended sessions and active sessions so a process restart
    /// can recover any live thread via `session/load`.
    persisted: HashMap<String, String>,
    /// Serializes create/resume work per thread so rapid same-thread requests
    /// cannot race each other into duplicate `session/load` attempts.
    creating: HashMap<String, Arc<Mutex<()>>>,
    /// Per-session working directory overrides (from control directives).
    /// thread_key → canonical workspace path.
    session_workdirs: HashMap<String, String>,
    /// Per-session project bindings: thread_key → canonical
    /// (project_id, project_root). Populated only when an incoming
    /// `ProjectContext` carries a non-empty `project_id`; anonymous
    /// `[[ws:@alias]]` workspace hints are intentionally absent so they
    /// don't pin a session to a project_id. Mismatch against an existing
    /// binding is the staleness gate; see
    /// [`SessionPool::get_or_create`] and workflow
    /// `20260818-openab-project-scoped-acp-session-bootstrap`.
    session_projects: HashMap<String, ProjectContext>,
}

pub struct SessionPool {
    state: RwLock<PoolState>,
    config: AgentConfig,
    max_sessions: usize,
    /// Force-evict sessions stuck in-flight longer than this threshold
    /// (`prompt_hard_timeout_secs + hung_grace_secs`, wired in main.rs).
    hung_threshold_secs: u64,
    mapping_path: PathBuf,
    meta_path: PathBuf,
    projects_path: PathBuf,
    default_config_options: HashMap<String, String>,
    /// Per-key set of thread_keys whose required project binding is
    /// UNTRUSTED. A key enters this set ONLY when `session_projects.json`
    /// failed to deserialize at startup — at that point, every key in
    /// `state.persisted` / `state.suspended` MIGHT have been pinned to a
    /// different project, and we cannot tell which without the lost data.
    /// A key is REMOVED from this set ONLY by:
    ///   - `reset_session(K)` / `purge_session_entries(K)` (the key is
    ///     dropped, so its untrusted state is no longer relevant); or
    ///   - a successful pinned `get_or_create(K, ...)` that persists a
    ///     NEW trusted binding for K (the per-key remove happens in the
    ///     post-spawn section, atomically with the in-memory insert).
    ///
    /// Critically, `save_projects` does NOT clear any global flag here:
    /// saving an unrelated fresh session C MUST NOT make a different
    /// untrusted key B "trusted" again. The previous design's
    /// `projects_corrupt: AtomicBool` had exactly that defect
    /// (workflow 20260818-openab-project-session-pinning-hardening,
    /// confirmed VERIFIER_FAIL on the second correction cycle).
    untrusted_project_keys: RwLock<HashSet<String>>,
    #[cfg(feature = "acp-mcp")]
    session_registrar: Option<Arc<dyn crate::acp_mcp::SessionTokenRegistrar>>,
    #[cfg(feature = "acp-mcp")]
    facade_url: Option<String>,
}

type CancelHandle = (Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>, String);
type ActiveSnapshot = Vec<(String, Arc<Mutex<AcpConnection>>)>;
type EvictionCandidate = (String, Arc<Mutex<AcpConnection>>, Instant, Option<String>);

fn remove_if_same_handle<T>(
    map: &mut HashMap<String, Arc<Mutex<T>>>,
    key: &str,
    expected: &Arc<Mutex<T>>,
) -> Option<Arc<Mutex<T>>> {
    let should_remove = map
        .get(key)
        .is_some_and(|current| Arc::ptr_eq(current, expected));
    if should_remove {
        map.remove(key)
    } else {
        None
    }
}

fn get_or_insert_gate(map: &mut HashMap<String, Arc<Mutex<()>>>, key: &str) -> Arc<Mutex<()>> {
    map.entry(key.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Returns true when a session should be treated as stale during idle cleanup.
fn classify_idle(last_active: Instant, alive: bool, cutoff: Instant) -> bool {
    last_active < cutoff || !alive
}

/// Returns true when a locked, in-flight session has exceeded the hung threshold.
fn classify_hung(
    in_flight: bool,
    last_active_age: std::time::Duration,
    threshold: std::time::Duration,
) -> bool {
    in_flight && last_active_age > threshold
}

/// Emit the force-evict warning with **both** ids redacted.
///
/// `key` is a pool key `<platform>:<channel_id>` (`acp_<uuid>`) and `session_id` is `sess_<uuid>`;
/// either resumes the session, so both are credentials. Extracted from the loop in `cleanup_idle`
/// so the redaction can be exercised by a test for real — R1 redacted the sites it enumerated and
/// this force-evict site was outside that list, logging both ids raw.
fn warn_force_evicting_hung(key: &str, session_id: Option<&str>, age_secs: u64, threshold_secs: u64) {
    warn!(
        thread_id = %crate::redact::redact_session_ids(key),
        session_id = %session_id.map(crate::redact::redact_session_ids).unwrap_or_default(),
        age_secs,
        threshold_secs,
        "force-evicting hung session"
    );
}

/// Returns true when `candidate_last_active` is a better eviction target than `current_oldest`.
fn better_candidate(current_oldest: Option<Instant>, candidate_last_active: Instant) -> bool {
    match current_oldest {
        Some(oldest) => candidate_last_active < oldest,
        None => true,
    }
}

/// Prepare facade browser capabilities for one session: write the agent's facade MCP entry, and
/// mint its session token **only if that write succeeded**.
///
/// The token is useless without the config. The file carries
/// `Authorization: Bearer ${OPENAB_SESSION_TOKEN}`, and it is the artifact the OPERATOR wires in
/// — since D-15 openab writes only `.openab/mcp-facade.json`, which no agent reads on its own, so
/// the import or `--mcp-config` flag is what actually points the agent at the facade. The ordering
/// still holds for a narrower reason: if openab cannot even author that file, the session has no
/// path to the facade it could be wired to, and minting regardless would register a live
/// credential for a session that cannot use it and leave it valid until eviction, while the
/// failure showed up only as a warning. Returning `None` keeps the session running without
/// browser capabilities, which is the honest description of what actually happened.
#[cfg(feature = "acp-mcp")]
async fn setup_facade_session(
    workdir: &str,
    facade_url: &str,
    channel_id: &str,
    registrar: &Arc<dyn crate::acp_mcp::SessionTokenRegistrar>,
) -> Option<String> {
    match crate::acp_mcp::write_facade_mcp_config(workdir, facade_url).await {
        Ok(()) => Some(registrar.mint(channel_id)),
        Err(e) => {
            tracing::error!(
                workdir, error = %e,
                "facade mcp config write failed — starting this session WITHOUT browser \
                 capabilities and not minting a session token that could never be presented"
            );
            None
        }
    }
}

/// Canonicalize a non-anonymous `ProjectContext` ONCE so the mismatch gate
/// and the workdir resolution both work from the same byte-equal form.
/// Returns `Ok(None)` for `None` and for anonymous contexts (no canonical
/// form, no mismatch check). Returns `Ok(Some(canonical))` for a valid
/// pinned context, or `Err(_)` for an invalid pinned path (nonexistent /
/// not a directory). The error string is propagated to the caller verbatim.
fn canonicalize_pinned(
    project: Option<&ProjectContext>,
) -> Result<Option<ProjectContext>, String> {
    match project {
        Some(p) if !p.is_anonymous() => p.canonicalized().map(Some),
        _ => Ok(None),
    }
}

/// Resolve the effective working directory for a session given the incoming
/// `ProjectContext`, the already-canonicalized pinned form (if any), the
/// stored per-session override (if any), and the configured
/// `[agent].working_dir`. Returns the resolved workdir and the project
/// binding (if any) to persist after a successful spawn.
///
/// Precedence (matches workflow req #6):
///   1. **Project-pinned** — when `canonical_pinned` is `Some`, its
///      `project_root` is authoritative; the canonical binding is returned
///      for persistence so the mismatch gate can later detect a stale
///      cross-project session.
///   2. **Stored** — per-thread immutability (ADR §4.5). When the incoming
///      context is anonymous, the existing workspace sticks; when no
///      binding exists yet, the anonymous path is used.
///   3. **Configured** — `[agent].working_dir`. The legacy single-project
///      fallback.
fn resolve_effective_workdir(
    project: Option<&ProjectContext>,
    canonical_pinned: Option<&ProjectContext>,
    stored_workdir: Option<&str>,
    config_workdir: &str,
) -> (String, Option<ProjectContext>) {
    if let Some(canonical) = canonical_pinned {
        return (
            canonical.project_root.to_string_lossy().to_string(),
            Some(canonical.clone()),
        );
    }

    if let Some(p) = project {
        if p.is_anonymous() {
            // Anonymous context (legacy `[[ws:@alias]]`): stored wins over the
            // workspace hint so the immutability invariant holds. When no
            // binding exists yet, use the anonymous path. Callers are expected
            // to have validated the path already (resolve_workspace does so);
            // we do not re-canonicalize because callers may pass a path that
            // is intentionally not on disk yet.
            if let Some(stored) = stored_workdir {
                return (stored.to_string(), None);
            }
            return (p.project_root.to_string_lossy().to_string(), None);
        }
    }

    let wd = stored_workdir.unwrap_or(config_workdir).to_string();
    (wd, None)
}
///
/// The single implementation for both hung eviction and [`SessionPool::reset_session`]; the latter
/// removes `active` itself and then calls this. It used to be a second copy of the same list, which
/// is how the two could drift — and the line most likely to be lost from a copy is the one below
/// about the creating gate, because it says *not* to remove something.
///
/// Hung eviction must NOT leave the session resumable: the old streaming task still holds an Arc
/// clone of the connection, so the agent process may be alive and mid-turn. If the session id
/// stayed in `suspended`/`persisted`, the next message would `session/load` the same session while
/// the old process still owns an in-flight turn.
fn purge_session_entries(state: &mut PoolState, key: &str) {
    state.cancel_handles.remove(key);
    state.activity.remove(key);
    state.pgids.remove(key);
    state.suspended.remove(key);
    state.persisted.remove(key);
    // Do NOT remove the creating gate: it is concurrency control, not session
    // state. Removing it while a holder still owns the old gate Arc would let
    // a concurrent get_or_create mint a fresh gate and run two creations for
    // the same key.
    state.session_workdirs.remove(key);
    // Project bindings must be cleared alongside session_workdirs so a
    // re-acquired thread key cannot accidentally inherit a stale project
    // identity from a previous (different) session for the same key. The
    // mismatch check is the live gate; persistence is the cross-restart one.
    state.session_projects.remove(key);
}

/// Escalating kill for a hung agent's process group: wait 10s after the
/// session/cancel attempt, SIGTERM, wait 2s, SIGKILL. Mirrors
/// `AcpConnection::kill_process_group`, which cannot run here because the
/// hung task never drops its connection Arc.
async fn kill_pgid_after_grace(pgid: Option<i32>) {
    let Some(pgid) = pgid.filter(|p| *p > 0) else {
        return;
    };
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    #[cfg(unix)]
    {
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        // No process-group kill on non-unix; rely on AcpConnection::Drop's
        // Windows handling if/when the hung task eventually unwinds.
        let _ = pgid;
    }
}

/// Remove a hung session from all pool maps. Returns true if the exact
/// connection captured at classification time was still registered; when a
/// fresh replacement exists for the key, nothing is touched.
///
/// Note: this helper intentionally does NOT touch
/// `untrusted_project_keys`. The caller (the cleanup_idle loop) holds
/// `&SessionPool` and is responsible for the per-key untrusted removal
/// AFTER `apply_hung_eviction` succeeds, so the `RwLock` write is taken
/// only when the eviction actually happened.
fn apply_hung_eviction(
    state: &mut PoolState,
    key: &str,
    expected: &Arc<Mutex<AcpConnection>>,
) -> bool {
    if remove_if_same_handle(&mut state.active, key, expected).is_none() {
        return false;
    }
    purge_session_entries(state, key);
    true
}

/// Record `token` as the facade token for `key`, revoking whatever token it supersedes.
///
/// A superseded token belongs to a predecessor connection under the same key. Its `AcpConnection`
/// DropGuard normally revokes it, but if that predecessor is hung (a stuck streaming task still
/// holds an Arc) the guard never fires — so revoking the superseded token here is what stops it
/// staying valid for the channel after a successor takes over (F3). Revocation is by exact token
/// and idempotent, so overlapping with the guard on a clean replacement is harmless.
#[cfg(feature = "acp-mcp")]
fn install_facade_token(
    state: &mut PoolState,
    key: &str,
    token: String,
    registrar: Option<&Arc<dyn crate::acp_mcp::SessionTokenRegistrar>>,
) {
    if let Some(superseded) = state.facade_tokens.insert(key.to_string(), token) {
        if let Some(registrar) = registrar {
            registrar.revoke(&superseded);
        }
    }
}

/// Revoke and forget the facade token recorded for `key`, if any.
///
/// Called from every path that removes a connection from `active` (hung eviction, idle eviction,
/// reset, suspend). On the clean paths the connection also drops and its guard revokes the same
/// token — idempotent — but the hung path is the one that needs this: the guard cannot fire while
/// the hung task holds an Arc, so without a synchronous revoke here the token outlives the eviction
/// and `AcpTunnelSource` (channel-only authorization) would let the hung predecessor reach a
/// successor's tunnel (F3). `purge_session_entries` deliberately does NOT touch `facade_tokens`, so
/// this can run *after* `apply_hung_eviction` and still find the token to revoke.
#[cfg(feature = "acp-mcp")]
fn revoke_facade_token_for_key(
    state: &mut PoolState,
    key: &str,
    registrar: Option<&Arc<dyn crate::acp_mcp::SessionTokenRegistrar>>,
) {
    if let Some(token) = state.facade_tokens.remove(key) {
        if let Some(registrar) = registrar {
            registrar.revoke(&token);
        }
    }
}

impl SessionPool {
    pub fn new(
        config: AgentConfig,
        max_sessions: usize,
        hung_threshold_secs: u64,
        default_config_options: HashMap<String, String>,
    ) -> Self {
        let openab_dir = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"))
            .join(".openab");
        let _ = std::fs::create_dir_all(&openab_dir);
        let mapping_path = openab_dir.join("thread_map.json");
        let meta_path = openab_dir.join("session_meta.json");
        let projects_path = openab_dir.join("session_projects.json");
        let suspended = Self::load_mapping(&mapping_path);
        let session_workdirs = Self::load_mapping(&meta_path);
        // Fail-closed for project-binding persistence (Defect 4 of workflow
        // 20260818-openab-project-session-pinning-hardening, refined in the
        // second correction cycle). A corrupt session_projects.json MUST NOT
        // silently become "no project bindings" — that would let
        // `get_or_create` reuse a persisted session whose original project
        // identity was lost.
        //
        // The previous design used a single global `projects_corrupt: bool`
        // and let `save_projects` clear it on any successful write. That
        // made the corruption guard vacuous: a fresh pinned session C would
        // clear the flag, and old untrusted key B would silently resume
        // afterward. The fix is per-key: every key in `persisted` /
        // `suspended` at the time of corruption is marked untrusted, and
        // each one is only removed from the untrusted set by its own
        // reset / purge / successful pinned-save path.
        let (session_projects, untrusted_keys) = match Self::load_projects(&projects_path) {
            Ok(map) => (map, HashSet::new()),
            Err(e) => {
                let keys: HashSet<String> = suspended.keys().cloned().collect();
                warn!(
                    path = %projects_path.display(),
                    error = %e,
                    untrusted_key_count = keys.len(),
                    "corrupt session_projects file — every persisted/suspended thread_key is \
                     marked UNTRUSTED individually; project-pinned get_or_create for each \
                     of those keys will fail closed until that key is reset or re-pinned"
                );
                (HashMap::new(), keys)
            }
        };
        Self {
            state: RwLock::new(PoolState {
                active: HashMap::new(),
                cancel_handles: HashMap::new(),
                #[cfg(feature = "acp-mcp")]
                facade_tokens: HashMap::new(),
                activity: HashMap::new(),
                pgids: HashMap::new(),
                persisted: suspended.clone(),
                suspended,
                creating: HashMap::new(),
                session_workdirs,
                session_projects,
            }),
            config,
            max_sessions,
            hung_threshold_secs,
            mapping_path,
            meta_path,
            projects_path,
            default_config_options,
            untrusted_project_keys: RwLock::new(untrusted_keys),
            #[cfg(feature = "acp-mcp")]
            session_registrar: None,
            #[cfg(feature = "acp-mcp")]
            facade_url: None,
        }
    }

    /// Wire the facade session-token registrar + facade URL, set by the root
    /// when `[mcp]` is running. With both present the pool does its half: mints
    /// one token per session, injects it as `OPENAB_SESSION_TOKEN` in the agent
    /// process env, and writes the static facade MCP entry once per workdir.
    ///
    /// That is necessary but NOT sufficient for browser capabilities to route
    /// through the facade. The operator must still put the written entry in front
    /// of the agent, and a `type:acp` server must actually attach over `/acp` —
    /// admission is that transport auth, not a config allowlist (D-29 removed
    /// `[[mcp.acp_servers]]`, reversing D-20).
    #[cfg(feature = "acp-mcp")]
    pub fn with_facade_sessions(
        mut self,
        registrar: Option<Arc<dyn crate::acp_mcp::SessionTokenRegistrar>>,
        facade_url: Option<String>,
    ) -> Self {
        self.session_registrar = registrar;
        self.facade_url = facade_url;
        self
    }

    /// Test-only constructor: build a `SessionPool` with a pre-populated
    /// `PoolState`. Bypasses the real filesystem persistence path so tests
    /// can drive `get_or_create` directly with specific thread_key →
    /// project bindings. Compiled out of release builds.
    #[cfg(test)]
    fn with_state_for_test(config: AgentConfig, state: PoolState, projects_path: PathBuf) -> Self {
        Self {
            state: RwLock::new(state),
            config,
            max_sessions: 4,
            hung_threshold_secs: 600,
            mapping_path: projects_path
                .parent()
                .unwrap_or(Path::new("/tmp"))
                .join("thread_map.json"),
            meta_path: projects_path
                .parent()
                .unwrap_or(Path::new("/tmp"))
                .join("session_meta.json"),
            projects_path,
            default_config_options: HashMap::new(),
            untrusted_project_keys: RwLock::new(HashSet::new()),
            #[cfg(feature = "acp-mcp")]
            session_registrar: None,
            #[cfg(feature = "acp-mcp")]
            facade_url: None,
        }
    }

    /// Test-only seam: replace the untrusted-key set so a test can drive
    /// the fail-closed path of `get_or_create` per-key, without writing
    /// to `$HOME/.openab/session_projects.json` (which would race other
    /// tests). Replaces the previous `set_projects_corrupt_for_test(bool)`
    /// seam that drove the (now-removed) global flag.
    /// Compiled out of release builds.
    #[cfg(test)]
    async fn set_untrusted_keys_for_test<I: IntoIterator<Item = String>>(&self, keys: I) {
        let mut set = self.untrusted_project_keys.write().await;
        set.clear();
        set.extend(keys);
    }

    fn load_mapping(path: &Path) -> HashMap<String, String> {
        match std::fs::read_to_string(path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_else(|e| {
                warn!(path = %path.display(), error = %e, "corrupt mapping file, starting fresh");
                HashMap::new()
            }),
            Err(_) => HashMap::new(),
        }
    }

    /// Load the per-session project bindings persisted at `path`.
    ///
    /// Returns `Ok(map)` on success. A missing file is `Ok(empty)` (first
    /// boot, normal). A *corrupt* file is `Err(_)` so the caller can set
    /// `projects_corrupt = true` and force project-pinned `get_or_create`
    /// to fail closed. The previous "corrupt file → empty map" behavior
    /// was unsafe (Defect 4 of workflow
    /// 20260818-openab-project-session-pinning-hardening): it silently
    /// demoted a partial or stale binding set to "no binding", which
    /// would have let the mismatch gate accept a `project B` call against
    /// a session whose original `project A` identity was just lost.
    fn load_projects(path: &Path) -> Result<HashMap<String, ProjectContext>, String> {
        let data = match std::fs::read_to_string(path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(HashMap::new());
            }
            Err(e) => {
                return Err(format!("read error: {e}"));
            }
        };
        serde_json::from_str(&data).map_err(|e| format!("json parse error: {e}"))
    }

    fn save_mapping(&self, persisted: &HashMap<String, String>) {
        let data = match serde_json::to_string_pretty(persisted) {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, "failed to serialize thread mapping");
                return;
            }
        };
        let tmp = self.mapping_path.with_extension("json.tmp");
        if let Err(e) =
            std::fs::write(&tmp, &data).and_then(|_| std::fs::rename(&tmp, &self.mapping_path))
        {
            warn!(path = %self.mapping_path.display(), error = %e, "failed to persist thread mapping");
        }
    }

    fn save_meta(&self, workdirs: &HashMap<String, String>) {
        let data = match serde_json::to_string_pretty(workdirs) {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, "failed to serialize session metadata");
                return;
            }
        };
        let tmp = self.meta_path.with_extension("json.tmp");
        if let Err(e) =
            std::fs::write(&tmp, &data).and_then(|_| std::fs::rename(&tmp, &self.meta_path))
        {
            warn!(path = %self.meta_path.display(), error = %e, "failed to persist session metadata");
        }
    }

    /// Persist per-session project bindings to `${HOME}/.openab/session_projects.json`.
    /// Mirrors `save_meta`'s atomic-write pattern (`.json.tmp` sibling + rename) so
    /// a crash mid-write cannot leave a half-written file behind to be mistaken for a
    /// real binding on the next startup.
    ///
    /// CRITICAL: this function does NOT clear any "corrupt" / "untrusted" state.
    /// The previous design's `save_projects` cleared a global `projects_corrupt`
    /// flag on every successful write, which made the corruption guard vacuous:
    /// persisting an unrelated fresh session C would let an OLD untrusted key B
    /// silently resume on the next call. The untrusted-key set is now per-key
    /// and is removed only by the per-key reset/purge path or the per-key
    /// post-spawn save of that specific key's new binding. `save_projects`
    /// remains a pure "write the in-memory map to disk" operation.
    fn save_projects(&self, projects: &HashMap<String, ProjectContext>) {
        let data = match serde_json::to_string_pretty(projects) {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, "failed to serialize session projects");
                return;
            }
        };
        let tmp = self.projects_path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, &data)
            .and_then(|_| std::fs::rename(&tmp, &self.projects_path))
        {
            warn!(
                path = %self.projects_path.display(),
                error = %e,
                "failed to persist session projects"
            );
        }
    }

    /// Check if session state exists for this thread (active, suspended, or persisted).
    #[allow(dead_code)]
    pub async fn has_active_session(&self, thread_id: &str) -> bool {
        let state = self.state.read().await;
        // Any of these means the thread already has session state.
        if state.suspended.contains_key(thread_id) || state.persisted.contains_key(thread_id) {
            return true;
        }
        if let Some(conn) = state.active.get(thread_id) {
            match conn.try_lock() {
                Ok(c) => return c.alive(),
                Err(_) => return true, // lock held = connection busy streaming = alive
            }
        }
        false
    }

    pub async fn get_or_create(
        &self,
        thread_id: &str,
        project: Option<&ProjectContext>,
    ) -> Result<bool> {
        let create_gate = {
            let mut state = self.state.write().await;
            get_or_insert_gate(&mut state.creating, thread_id)
        };
        let _create_guard = create_gate.lock().await;

        // ── Phase 1: snapshot every state slice this decision needs ────────────
        //
        // Read active connection, suspended sessionId, AND stored project
        // binding for this thread in a single read lock. Hoisting
        // `session_projects` here is what lets the mismatch gate run
        // BEFORE the busy/alive/resume fast paths below (Defect 1 of
        // workflow 20260818-openab-project-session-pinning-hardening).
        let (existing, saved_session_id, stored_project) = {
            let state = self.state.read().await;
            (
                state.active.get(thread_id).cloned(),
                state.suspended.get(thread_id).cloned(),
                state.session_projects.get(thread_id).cloned(),
            )
        };

        // ── Phase 2: canonicalize the incoming project ONCE ────────────────────
        //
        // For pinned (non-anonymous) contexts this validates the path and
        // produces a canonical form. The canonical form is the same one
        // the stored binding was stored as, so the mismatch check below
        // is a byte-equal comparison and the workdir resolution uses the
        // same path the agent will see as `cwd`. Anonymous and absent
        // contexts yield `None` — no canonical form, no mismatch check.
        // Any invalid-pinned-path error is surfaced here, BEFORE any
        // fast path, so a typo in a project_id never silently falls
        // through to the configured working directory.
        let canonical_pinned = canonicalize_pinned(project).map_err(anyhow::Error::msg)?;

        // ── Phase 3: FAIL-CLOSED PROJECT MISMATCH GATE ────────────────────────
        //
        // Runs BEFORE the busy / alive / resumed fast paths so a stale
        // cross-project active session cannot be silently reused. The
        // previous ordering checked mismatch AFTER the connection
        // fast paths, so `active project A + incoming project B` could
        // return `Ok(false)` and reuse A without ever comparing the
        // contexts. Comparison is on the canonical form produced in
        // Phase 2 and the canonical form stored at persist time, so
        // `"/a"` vs `"/a/"` vs `"/a/./"` all compare equal (canonical
        // equivalence is preserved).
        if let Some(incoming) = &canonical_pinned {
            if let Some(stored) = &stored_project {
                if stored.project_id != incoming.project_id
                    || stored.project_root != incoming.project_root
                {
                    warn!(
                        thread_id = %crate::redact::redact_session_ids(thread_id),
                        stored_project_id = %stored.project_id,
                        incoming_project_id = %incoming.project_id,
                        "project context mismatch — refusing to reuse stale session"
                    );
                    return Err(anyhow!(
                        "project context mismatch: existing session for {} is bound to \
                         project_id={:?} project_root={:?}, but incoming context is \
                         project_id={:?} project_root={:?}; refuse to reuse a stale \
                         cross-project session",
                        crate::redact::redact_session_ids(thread_id),
                        stored.project_id,
                        stored.project_root,
                        incoming.project_id,
                        incoming.project_root,
                    ));
                }
            }
        }

        // ── Phase 4: fail-closed for untrusted per-key project binding ────────
        //
        // The `untrusted_project_keys` set is populated ONLY by
        // `SessionPool::new()` when `load_projects` returned `Err`. In that
        // case, EVERY key in `state.persisted` / `state.suspended` at
        // startup was added to the set, because we cannot tell which of
        // them was project-pinned to which project without the lost
        // binding data. For each such key, a pinned `get_or_create`
        // must fail closed: the original project identity of the
        // resumable session is unknown, and reusing it would be a
        // potentially cross-project silent reuse.
        //
        // A key leaves the untrusted set ONLY through:
        //   - `reset_session(K)` / `purge_session_entries(K)` (key gone);
        //   - the post-spawn section below, when a successful pinned
        //     `get_or_create(K, ...)` persists a NEW trusted binding
        //     for K (the per-key remove happens atomically there).
        //
        // Saving an UNRELATED key C does NOT remove key B from the
        // untrusted set — the previous global-flag design had exactly
        // that defect (verified by the second VERIFIER cycle).
        if canonical_pinned.is_some() {
            let is_untrusted = {
                let set = self.untrusted_project_keys.read().await;
                set.contains(thread_id)
            };
            if is_untrusted {
                let has_persisted = {
                    let state = self.state.read().await;
                    state.persisted.contains_key(thread_id)
                        || state.suspended.contains_key(thread_id)
                };
                if has_persisted {
                    warn!(
                        thread_id = %crate::redact::redact_session_ids(thread_id),
                        "project binding for this thread is untrusted (session_projects \
                         persistence was corrupt at startup); refusing to resume — caller \
                         must reset and retry"
                    );
                    return Err(anyhow!(
                        "project binding for {} is untrusted (session_projects persistence \
                         was corrupt at startup); refusing to resume. Reset the session and \
                         retry with a trusted project context.",
                        crate::redact::redact_session_ids(thread_id),
                    ));
                }
                // No persisted mapping — fresh key, even though the file
                // was corrupt. Allow it: there is no cross-project reuse
                // risk to gate. (Defect 4 required invariant: "do not
                // globally block all future fresh sessions if they have
                // no persisted / suspended mapping".)
            }
        }

        // ── Phase 5: existing-connection fast paths (busy / alive / resume) ──
        //
        // The mismatch gate above has already verified the contexts are
        // compatible, so it is safe to short-circuit when the existing
        // connection is busy or alive. Lock held = busy streaming =
        // alive (same convention as `has_active_session`); cleanup_idle
        // owns hung recovery. Never await the connection's mutex here
        // while holding `create_gate` — F1.
        let had_existing = existing.is_some();
        let mut saved_session_id = saved_session_id;
        if let Some(conn) = existing.clone() {
            let Ok(conn) = conn.try_lock() else {
                return Ok(false);
            };
            if conn.alive() {
                return Ok(false);
            }
            if saved_session_id.is_none() {
                saved_session_id = conn.acp_session_id.clone();
            }
        }

        // Snapshot active handles so we can inspect them outside the state lock.
        let snapshot: Vec<(String, Arc<Mutex<AcpConnection>>)> = {
            let state = self.state.read().await;
            state
                .active
                .iter()
                .map(|(k, v)| (k.clone(), Arc::clone(v)))
                .collect()
        };

        let mut eviction_candidate: Option<EvictionCandidate> = None;
        let mut skipped_locked_candidates = 0usize;
        for (key, conn) in snapshot {
            if key == thread_id {
                continue;
            }
            let conn_handle = Arc::clone(&conn);
            let Ok(conn) = conn.try_lock() else {
                skipped_locked_candidates += 1;
                continue;
            };
            let candidate = (
                key,
                conn_handle,
                conn.last_active,
                conn.acp_session_id.clone(),
            );
            if better_candidate(
                eviction_candidate.as_ref().map(|(_, _, t, _)| *t),
                candidate.2,
            ) {
                eviction_candidate = Some(candidate);
            }
        }

        // Resolve effective working directory.
        //
        // Precedence (matches workflow req #6 + ADR §4.5 immutability):
        //   1. project-pinned project_root when the incoming ProjectContext carries
        //      a non-empty project_id. Authoritative: the session is bound to this
        //      (project_id, project_root) for its lifetime.
        //   2. Stored per-session workdir (legacy immutability — once a workspace
        //      sticks for a thread it stays, even if a later call forgets the
        //      override).
        //   3. Configured [agent].working_dir.
        //
        // Anonymous contexts (`project_id.is_empty()`) preserve the legacy
        // `[[ws:@alias]]` semantics: stored wins over the workspace hint, so an
        // existing session is not silently moved by a new directive in a later
        // message (ADR §4.5). The anonymous path itself is used as the workdir
        // only when no stored binding exists.
        let stored_workdir = {
            let state = self.state.read().await;
            state.session_workdirs.get(thread_id).cloned()
        };

        // Use the pre-canonicalized form from Phase 2. resolve_effective_workdir
        // no longer re-validates the path; the canonical form is the
        // authoritative workdir AND the binding we will persist.
        let (effective_workdir, project_to_store) = resolve_effective_workdir(
            project,
            canonical_pinned.as_ref(),
            stored_workdir.as_deref(),
            &self.config.working_dir,
        );

        // Browser capabilities for an `acp:` session come from the OAB MCP Facade and nowhere
        // else: mint a per-session token (it rides the agent spawn below as OPENAB_SESSION_TOKEN)
        // and write the static facade entry before the agent boots. The returned guard revokes
        // that token when this connection is dropped, on any evict path.
        //
        // There is no transport fallback. Without `[mcp]` the root wires no registrar, and the
        // session simply starts without browser capabilities — which is the honest outcome and is
        // reported once at startup rather than being silently substituted per session.
        #[cfg(feature = "acp-mcp")]
        let mut session_token: Option<String> = None;
        #[cfg(feature = "acp-mcp")]
        let facade_token_guard: Option<tokio_util::sync::DropGuard> = match (
            thread_id.strip_prefix("acp:"),
            self.session_registrar.as_ref(),
            self.facade_url.as_ref(),
        ) {
            (Some(channel_id), Some(registrar), Some(facade_url)) => {
                match setup_facade_session(&effective_workdir, facade_url, channel_id, registrar)
                    .await
                {
                    Some(token) => {
                        session_token = Some(token.clone());
                        info!(thread_id = %crate::redact::redact_session_ids(thread_id), "session token minted for facade browser capabilities");
                        // The guard carries the TOKEN it minted, not the channel. A replaced
                        // session's teardown runs after its successor has already re-minted for
                        // the same channel, so revoking by channel would strip the live token and
                        // silently cut the new agent off from the facade; revoking this exact
                        // token is a no-op by then (R1).
                        let ct = tokio_util::sync::CancellationToken::new();
                        let child = ct.child_token();
                        let registrar = registrar.clone();
                        tokio::spawn(async move {
                            child.cancelled().await;
                            registrar.revoke(&token);
                        });
                        Some(ct.drop_guard())
                    }
                    // No config, so no token and no revoke guard to arm. The session still
                    // starts — it simply has no browser capabilities.
                    None => None,
                }
            }
            _ => None,
        };

        // Build the replacement connection outside the state lock so one stuck
        // initialization does not block all unrelated sessions.
        #[cfg(feature = "acp-mcp")]
        let spawn_env: std::collections::HashMap<String, String> = {
            let mut env = self.config.env.clone();
            if let Some(tok) = &session_token {
                // The static facade MCP entry references ${OPENAB_SESSION_TOKEN};
                // the value lives only in this agent process's environment.
                env.insert("OPENAB_SESSION_TOKEN".to_string(), tok.clone());
            }
            env
        };
        #[cfg(not(feature = "acp-mcp"))]
        let spawn_env = self.config.env.clone();
        let mut new_conn = AcpConnection::spawn(
            &self.config.command,
            &self.config.args,
            &effective_workdir,
            &spawn_env,
            &self.config.inherit_env,
        )
        .await?;

        new_conn.initialize().await?;

        let mut resumed = false;
        let mut load_failed: Option<&str> = None;
        if let Some(ref sid) = saved_session_id {
            if new_conn.supports_load_session {
                match new_conn.session_load(sid, &effective_workdir).await {
                    Ok(()) => {
                        info!(thread_id = %crate::redact::redact_session_ids(thread_id), session_id = %crate::redact::redact_session_ids(sid), "session resumed via session/load");
                        resumed = true;
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        let is_transient =
                            TRANSIENT_LOAD_ERRORS.iter().any(|s| err_str.contains(s));
                        if is_transient {
                            warn!(thread_id = %crate::redact::redact_session_ids(thread_id), session_id = %crate::redact::redact_session_ids(sid), error = %e,
                                "session/load failed transiently, preserving session ID for retry");
                            load_failed = Some(if err_str.contains("timeout waiting for") {
                                "timeout"
                            } else {
                                "connection lost"
                            });
                        } else {
                            warn!(thread_id = %crate::redact::redact_session_ids(thread_id), session_id = %crate::redact::redact_session_ids(sid), error = %e,
                                "session/load failed, creating new session");
                        }
                    }
                }
            }
        }

        if let Some(reason) = load_failed {
            // session/load failed transiently. The original session ID is already
            // in state.persisted (we haven't touched it), so the next message will
            // retry session/load automatically. Return an error so the current message
            // is not processed against a context-free session.
            return Err(anyhow!(
                "session load {reason}: could not restore previous session"
            ));
        }

        if !resumed {
            new_conn.session_new(&effective_workdir).await?;

            // Apply default config options (e.g. mode=bypass, model=swe-1-6)
            for (config_id, value) in &self.default_config_options {
                if let Err(e) = new_conn.set_config_option(config_id, value).await {
                    warn!(config_id, value, error = %e, "failed to set default config option");
                }
            }

            // Surface the reset banner both for restored sessions and for stale
            // live entries that died before we could recover a resumable
            // session id. In both cases the caller is continuing after an
            // unexpected session loss.
            if had_existing || saved_session_id.is_some() {
                new_conn.session_reset = true;
            }
        }

        let cancel_handle = new_conn.cancel_handle();
        let activity_handle = new_conn.activity_handle();
        let child_pgid = new_conn.child_pgid();
        let cancel_session_id = new_conn.acp_session_id.clone().unwrap_or_default();
        #[cfg(feature = "acp-mcp")]
        new_conn.set_facade_token_guard(facade_token_guard);
        let new_conn = Arc::new(Mutex::new(new_conn));

        let mut state = self.state.write().await;

        // Another task may have created a healthy connection while we were
        // initializing this one.
        if let Some(existing) = state.active.get(thread_id).cloned() {
            let Ok(existing) = existing.try_lock() else {
                return Ok(false);
            };
            if existing.alive() {
                return Ok(false);
            }
            warn!(thread_id = %crate::redact::redact_session_ids(thread_id), "stale connection, rebuilding");
            drop(existing);
            state.active.remove(thread_id);
            state.cancel_handles.remove(thread_id);
            state.activity.remove(thread_id);
            state.pgids.remove(thread_id);
        }

        if state.active.len() >= self.max_sessions {
            if let Some((key, expected_conn, _, sid)) = eviction_candidate {
                if remove_if_same_handle(&mut state.active, &key, &expected_conn).is_some() {
                    state.cancel_handles.remove(&key);
                    state.activity.remove(&key);
                    state.pgids.remove(&key);
                    #[cfg(feature = "acp-mcp")]
                    revoke_facade_token_for_key(&mut state, &key, self.session_registrar.as_ref());
                    info!(evicted = %crate::redact::redact_session_ids(&key), "pool full, suspending oldest idle session");
                    if let Some(sid) = sid {
                        state.persisted.insert(key.clone(), sid.clone());
                        state.suspended.insert(key, sid);
                    } else {
                        state.persisted.remove(&key);
                    }
                } else {
                    warn!(evicted = %crate::redact::redact_session_ids(&key), "pool full but eviction candidate changed before removal");
                }
            } else if skipped_locked_candidates > 0 {
                warn!(
                    max_sessions = self.max_sessions,
                    skipped_locked_candidates,
                    "pool full but all other sessions were busy during eviction scan"
                );
            }
        }

        if state.active.len() >= self.max_sessions {
            return Err(anyhow!("pool exhausted ({} sessions)", self.max_sessions));
        }

        if cancel_session_id.is_empty() {
            state.persisted.remove(thread_id);
        } else {
            state
                .persisted
                .insert(thread_id.to_string(), cancel_session_id.clone());
        }
        state.suspended.remove(thread_id);
        state.active.insert(thread_id.to_string(), new_conn);
        state
            .activity
            .insert(thread_id.to_string(), activity_handle);
        if let Some(pgid) = child_pgid {
            state.pgids.insert(thread_id.to_string(), pgid);
        }
        if !cancel_session_id.is_empty() {
            state
                .cancel_handles
                .insert(thread_id.to_string(), (cancel_handle, cancel_session_id));
        }
        // Record this connection's exact token lock-free, revoking any predecessor token it
        // supersedes under the same key (its guard cannot fire if that predecessor is hung). F3.
        #[cfg(feature = "acp-mcp")]
        if let Some(token) = session_token {
            install_facade_token(&mut state, thread_id, token, self.session_registrar.as_ref());
        }
        self.save_mapping(&state.persisted);

        // Persist workspace override only after session spawn succeeded (口渡 F2).
        // Mirrors the original behavior: a thread's first workspace hint sticks
        // for the rest of the session's lifetime (ADR §4.5 immutability). When
        // a project-pinned context drove `effective_workdir`, persist the workdir
        // AND the project binding so a process restart does not drop the pin.
        if project.is_some() {
            state
                .session_workdirs
                .entry(thread_id.to_string())
                .or_insert_with(|| effective_workdir.clone());
            self.save_meta(&state.session_workdirs);
        }
        if let Some(p) = &project_to_store {
            state.session_projects.insert(thread_id.to_string(), p.clone());
            self.save_projects(&state.session_projects);
            // A fresh trusted binding for this specific key has just
            // been persisted. Remove the key from the untrusted set so
            // future pinned get_or_create calls for THIS key are no
            // longer fail-closed. Other untrusted keys (e.g. another
            // pre-existing key whose binding was lost) remain
            // untrusted; only this key's per-key state is cleared.
            self.untrusted_project_keys
                .write()
                .await
                .remove(thread_id);
        }

        // Return true only for genuinely new sessions — not resumed or reconnected ones.
        // A session with prior state (saved_session_id or had_existing) is a resume,
        // even if we had to spawn a new ACP process. ADR §2.2: directives are first-message-only.
        let is_fresh = !had_existing && saved_session_id.is_none();
        Ok(is_fresh)
    }

    /// Get mutable access to a connection. Caller must have called get_or_create first.
    ///
    /// Only the per-connection `Mutex` is held during `f`; the pool-level
    /// `RwLock` is acquired briefly (read-only) to look up the `Arc` and then
    /// released, so other connections can be used concurrently.
    pub async fn with_connection<F, R>(&self, thread_id: &str, f: F) -> Result<R>
    where
        F: for<'a> FnOnce(
            &'a mut AcpConnection,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<R>> + Send + 'a>,
        >,
    {
        let conn = {
            let state = self.state.read().await;
            state
                .active
                .get(thread_id)
                .cloned()
                .ok_or_else(|| anyhow!("no connection for thread {}", crate::redact::redact_session_ids(thread_id)))?
        };

        let mut conn = conn.lock().await;
        f(&mut conn).await
    }

    /// Get cached configOptions for a session (e.g. available models).
    pub async fn get_config_options(&self, thread_id: &str) -> Vec<ConfigOption> {
        let state = self.state.read().await;
        let conn = match state.active.get(thread_id) {
            Some(c) => c.clone(),
            None => return Vec::new(),
        };
        drop(state);
        let conn = conn.lock().await;
        conn.config_options.clone()
    }

    /// Set a config option (e.g. model) via ACP and return updated options.
    pub async fn set_config_option(
        &self,
        thread_id: &str,
        config_id: &str,
        value: &str,
    ) -> Result<Vec<ConfigOption>> {
        let conn = {
            let state = self.state.read().await;
            state
                .active
                .get(thread_id)
                .cloned()
                .ok_or_else(|| anyhow!("no connection for thread {}", crate::redact::redact_session_ids(thread_id)))?
        };
        let mut conn = conn.lock().await;
        conn.set_config_option(config_id, value).await
    }

    /// Query account-level usage/billing from the backend agent for a session
    /// (kiro-cli extension). Fails when there is no active session for the
    /// thread or the backend does not support usage queries.
    pub async fn get_usage(&self, thread_id: &str) -> Result<crate::acp::protocol::UsageReport> {
        let conn = {
            let state = self.state.read().await;
            state
                .active
                .get(thread_id)
                .cloned()
                .ok_or_else(|| anyhow!("no connection for thread {}", crate::redact::redact_session_ids(thread_id)))?
        };
        let mut conn = conn.lock().await;
        conn.get_usage().await
    }

    /// Cancel the current in-flight operation for a session.
    /// Uses pre-stored cancel handles to avoid locking the connection (which is held during streaming).
    pub async fn cancel_session(&self, thread_id: &str) -> Result<()> {
        let (stdin, session_id) = {
            let state = self.state.read().await;
            state
                .cancel_handles
                .get(thread_id)
                .cloned()
                .ok_or_else(|| anyhow!("no session for thread {}", crate::redact::redact_session_ids(thread_id)))?
        };
        let data = serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/cancel",
            "params": {"sessionId": session_id}
        }))?;
        tracing::info!(session_id = %crate::redact::redact_session_ids(&session_id), "sending session/cancel");
        use tokio::io::AsyncWriteExt;
        let mut w = stdin.lock().await;
        w.write_all(data.as_bytes()).await?;
        w.write_all(b"\n").await?;
        w.flush().await?;
        Ok(())
    }

    /// Reset a session: cancel any in-flight operation, remove the active connection,
    /// and clear all suspended state. The ACP process will be killed once the last
    /// Arc reference is dropped (after streaming finishes). The next message will
    /// trigger a fresh `get_or_create` with a new ACP session.
    pub async fn reset_session(&self, thread_id: &str) -> Result<()> {
        // Send session/cancel via the lock-free stdin handle first.
        // This stops in-flight streaming even while with_connection() holds the
        // connection mutex, so the old process finishes promptly.
        if let Some((stdin, session_id)) = {
            let state = self.state.read().await;
            state.cancel_handles.get(thread_id).cloned()
        } {
            let data = serde_json::to_string(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "session/cancel",
                "params": {"sessionId": session_id}
            }))?;
            tracing::info!(session_id = %crate::redact::redact_session_ids(&session_id), "reset: sending session/cancel");
            use tokio::io::AsyncWriteExt;
            let mut w = stdin.lock().await;
            let _ = w.write_all(data.as_bytes()).await;
            let _ = w.write_all(b"\n").await;
            let _ = w.flush().await;
        }

        let mut state = self.state.write().await;
        let had_active = state.active.remove(thread_id).is_some();
        // Everything else a reset clears is exactly what hung eviction clears, including the rule
        // that the creating gate survives. Call the one implementation rather than keeping a second
        // copy of the list: the copies are what let the two drift, and the gate rule is precisely
        // the kind of line that gets dropped from a duplicate without anyone noticing.
        purge_session_entries(&mut state, thread_id);
        // Resetting a hung session drops the map's Arc but not the one the stuck task holds, so the
        // guard cannot revoke — do it synchronously here too (F3).
        #[cfg(feature = "acp-mcp")]
        revoke_facade_token_for_key(&mut state, thread_id, self.session_registrar.as_ref());
        self.save_mapping(&state.persisted);
        self.save_meta(&state.session_workdirs);
        // Project binding must be cleared on reset so the next message can pin
        // a fresh project context. Without this, a reset under thread_key T
        // would carry over the previous session's project binding, and the
        // mismatch check would refuse the new context against the OLD pin.
        self.save_projects(&state.session_projects);
        // Drop the state write lock before touching the untrusted set.
        drop(state);
        // A reset removes this key's session entirely. The untrusted
        // marker is tied to "this key might have a wrong binding for
        // a resumable session"; once the session is gone, the marker
        // is no longer relevant — a subsequent fresh pinned
        // get_or_create under this key starts a clean slate.
        self.untrusted_project_keys
            .write()
            .await
            .remove(thread_id);
        if had_active {
            info!(thread_id = %crate::redact::redact_session_ids(thread_id), "session reset");
            Ok(())
        } else {
            Err(anyhow!("no session for thread {}", crate::redact::redact_session_ids(thread_id)))
        }
    }

    pub async fn cleanup_idle(&self, ttl_secs: u64) {
        let cutoff = Instant::now() - std::time::Duration::from_secs(ttl_secs);
        let hung_threshold = std::time::Duration::from_secs(self.hung_threshold_secs);

        let (snapshot, activity_map, cancel_map, pgid_map) = {
            let state = self.state.read().await;
            let snapshot: ActiveSnapshot = state
                .active
                .iter()
                .map(|(k, v)| (k.clone(), Arc::clone(v)))
                .collect();
            (
                snapshot,
                state.activity.clone(),
                state.cancel_handles.clone(),
                state.pgids.clone(),
            )
        };

        let mut stale = Vec::new();
        let mut hung: Vec<(String, Arc<Mutex<AcpConnection>>)> = Vec::new();
        for (key, conn) in snapshot {
            // Skip active sessions for this cleanup round instead of waiting on
            // their per-connection mutex. A busy session is not idle unless hung.
            let conn_handle = Arc::clone(&conn);
            let Ok(conn) = conn.try_lock() else {
                if let Some(activity) = activity_map.get(&key) {
                    if classify_hung(activity.in_flight(), activity.age(), hung_threshold) {
                        let session_id = cancel_map.get(&key).map(|(_, sid)| sid.clone());
                        warn_force_evicting_hung(
                            &key,
                            session_id.as_deref(),
                            activity.age().as_secs(),
                            self.hung_threshold_secs,
                        );
                        // Best-effort session/cancel via the lock-free stdin
                        // handle, detached so a wedged stdin can never block
                        // cleanup (and never while holding `state`). The hung
                        // task never unwinds, so AcpConnection::Drop never
                        // fires; after the cancel attempt, kill the child
                        // process group directly or the agent leaks forever (F4).
                        let stdin_handle = cancel_map.get(&key).map(|(stdin, _)| Arc::clone(stdin));
                        let pgid = pgid_map.get(&key).copied();
                        tokio::spawn(async move {
                            if let (Some(stdin), Some(session_id)) = (stdin_handle, session_id) {
                                let _ = tokio::time::timeout(
                                    std::time::Duration::from_secs(5),
                                    async move {
                                        if let Ok(data) =
                                            serde_json::to_string(&serde_json::json!({
                                                "jsonrpc": "2.0",
                                                "method": "session/cancel",
                                                "params": {"sessionId": session_id}
                                            }))
                                        {
                                            use tokio::io::AsyncWriteExt;
                                            let mut w = stdin.lock().await;
                                            let _ = w.write_all(data.as_bytes()).await;
                                            let _ = w.write_all(b"\n").await;
                                            let _ = w.flush().await;
                                        }
                                    },
                                )
                                .await;
                            }
                            kill_pgid_after_grace(pgid).await;
                        });
                        hung.push((key, conn_handle));
                    }
                }
                continue;
            };
            // try_lock success means no turn is streaming under
            // with_connection, so a true in_flight flag is stale (the turn
            // aborted without prompt_done). Self-heal it so the session can
            // never be falsely classified as hung later.
            if let Some(activity) = activity_map.get(&key) {
                if activity.in_flight() {
                    activity.set_in_flight(false);
                    activity.touch();
                }
            }
            if classify_idle(conn.last_active, conn.alive(), cutoff) {
                stale.push((key, conn_handle, conn.acp_session_id.clone()));
            }
        }

        if stale.is_empty() && hung.is_empty() {
            return;
        }

        let mut state = self.state.write().await;
        // Keys that were fully idle-evicted (no resumable sessionId
        // left) — their untrusted marker must be removed AFTER we drop
        // the state write lock, so we don't nest two write locks.
        let mut fully_evicted_keys: Vec<String> = Vec::new();
        for (key, expected_conn, sid) in stale {
            if remove_if_same_handle(&mut state.active, &key, &expected_conn).is_some() {
                info!(thread_id = %crate::redact::redact_session_ids(&key), "cleaning up idle session");
                state.cancel_handles.remove(&key);
                state.activity.remove(&key);
                state.pgids.remove(&key);
                #[cfg(feature = "acp-mcp")]
                revoke_facade_token_for_key(&mut state, &key, self.session_registrar.as_ref());
                if let Some(sid) = sid {
                    state.persisted.insert(key.clone(), sid.clone());
                    state.suspended.insert(key, sid);
                } else {
                    state.persisted.remove(&key);
                    state.session_workdirs.remove(&key);
                    // An idle-evicted session loses its project binding too:
                    // the session is gone, and a future get_or_create under the
                    // same thread_key must NOT inherit a project_id the new
                    // call did not ask for.
                    state.session_projects.remove(&key);
                    // The untrusted marker is also tied to "this key
                    // has a resumable session whose binding might be
                    // wrong". When the session is fully evicted (no
                    // resumable sessionId), the marker is no longer
                    // relevant — capture the key for post-state-lock
                    // removal below. We can't take the untrusted-set
                    // write lock while holding the state write lock.
                    fully_evicted_keys.push(key);
                }
            }
        }
        for (key, expected_conn) in hung {
            if apply_hung_eviction(&mut state, &key, &expected_conn) {
                // The DropGuard cannot fire — the hung streaming task still holds an Arc, so the
                // connection never drops. Revoke the exact token synchronously, or it keeps
                // resolving to the channel and a successor's tunnel becomes reachable by the hung
                // predecessor (F3). Safe after `apply_hung_eviction`: its `purge_session_entries`
                // leaves `facade_tokens` alone.
                #[cfg(feature = "acp-mcp")]
                revoke_facade_token_for_key(&mut state, &key, self.session_registrar.as_ref());
                // The key's project binding was just purged alongside the
                // session state. The untrusted marker is no longer
                // relevant: future calls under this key start fresh.
                // Defer the untrusted-set removal to AFTER the state
                // write lock is dropped (see post-loop block).
                fully_evicted_keys.push(key);
            } else {
                warn!(thread_id = %crate::redact::redact_session_ids(&key), "hung session was replaced before eviction; maps untouched");
            }
        }
        self.save_mapping(&state.persisted);
        self.save_meta(&state.session_workdirs);
        self.save_projects(&state.session_projects);
        // Drop the state write lock before touching the untrusted set
        // (avoid nested write locks — the `untrusted_project_keys`
        // RwLock is a separate lock that the gate consults and that
        // test seams write to).
        drop(state);
        if !fully_evicted_keys.is_empty() {
            let mut untrusted = self.untrusted_project_keys.write().await;
            for k in &fully_evicted_keys {
                untrusted.remove(k);
            }
        }
    }

    pub async fn shutdown(&self) {
        // Snapshot active handles, then drop state lock before awaiting
        // per-connection mutexes (lock ordering: never hold state while
        // awaiting a connection lock).
        let snapshot: Vec<(String, Arc<Mutex<AcpConnection>>)> = {
            let state = self.state.read().await;
            state
                .active
                .iter()
                .map(|(k, v)| (k.clone(), Arc::clone(v)))
                .collect()
        };

        let mut session_ids: Vec<(String, String)> = Vec::new();
        for (key, conn) in snapshot {
            let conn = conn.lock().await;
            if let Some(sid) = conn.acp_session_id.clone() {
                session_ids.push((key, sid));
            }
        }

        let mut state = self.state.write().await;
        for (key, sid) in session_ids {
            state.persisted.insert(key.clone(), sid.clone());
            state.suspended.insert(key, sid);
        }
        self.save_mapping(&state.persisted);
        let count = state.active.len();
        state.active.clear();
        state.cancel_handles.clear();
        state.activity.clear();
        state.pgids.clear();
        info!(count, "pool shutdown complete");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        better_candidate, classify_hung, classify_idle, get_or_insert_gate, purge_session_entries,
        remove_if_same_handle, PoolState, SessionPool,
    };
    use crate::acp::connection::SessionActivity;
    use crate::acp::project::ProjectContext;
    use crate::config::AgentConfig;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tokio::sync::Mutex;
    use tokio::time::Instant;

    /// Registrar double that records every mint, so a test can assert one never happened.
    #[cfg(feature = "acp-mcp")]
    #[derive(Default)]
    struct CountingRegistrar {
        minted: std::sync::Mutex<Vec<String>>,
        revoked: std::sync::Mutex<Vec<String>>,
    }

    #[cfg(feature = "acp-mcp")]
    impl CountingRegistrar {
        fn revoked(&self) -> Vec<String> {
            self.revoked.lock().unwrap().clone()
        }
    }

    #[cfg(feature = "acp-mcp")]
    impl crate::acp_mcp::SessionTokenRegistrar for CountingRegistrar {
        fn mint(&self, channel_id: &str) -> String {
            self.minted.lock().unwrap().push(channel_id.to_string());
            "token-xyz".to_string()
        }
        fn revoke(&self, token: &str) {
            self.revoked.lock().unwrap().push(token.to_string());
        }
    }

    /// Build an empty `PoolState` for a helper-level test.
    #[cfg(feature = "acp-mcp")]
    fn empty_pool_state() -> super::PoolState {
        super::PoolState {
            active: HashMap::new(),
            cancel_handles: HashMap::new(),
            facade_tokens: HashMap::new(),
            activity: HashMap::new(),
            pgids: HashMap::new(),
            suspended: HashMap::new(),
            persisted: HashMap::new(),
            creating: HashMap::new(),
            session_workdirs: HashMap::new(),
            session_projects: HashMap::new(),
        }
    }

    /// F3: replacing a hung predecessor's token revokes the predecessor's EXACT token and leaves
    /// the successor's standing. Without the revoke the predecessor token keeps resolving to the
    /// channel and — since `AcpTunnelSource` authorizes by channel — could reach the successor's
    /// tunnel. Exercises the production `install_facade_token`.
    #[cfg(feature = "acp-mcp")]
    #[test]
    fn installing_a_successor_token_revokes_only_the_superseded_predecessor() {
        let reg = Arc::new(CountingRegistrar::default());
        let registrar: Arc<dyn crate::acp_mcp::SessionTokenRegistrar> = reg.clone();
        let mut state = empty_pool_state();

        // Predecessor registers, then a successor takes over the SAME key.
        super::install_facade_token(&mut state, "discord:acp_x", "T_pred".into(), Some(&registrar));
        assert!(reg.revoked().is_empty(), "nothing to revoke on the first install");
        super::install_facade_token(&mut state, "discord:acp_x", "T_succ".into(), Some(&registrar));

        assert_eq!(reg.revoked(), vec!["T_pred"], "the predecessor token must be revoked");
        assert_eq!(
            state.facade_tokens.get("discord:acp_x").map(String::as_str),
            Some("T_succ"),
            "the successor's token stands"
        );
    }

    /// F3: hung eviction revokes the exact facade token synchronously (the DropGuard cannot fire
    /// while the hung task holds an Arc). Exercises the production `revoke_facade_token_for_key`,
    /// which the hung-eviction loop calls after `apply_hung_eviction`.
    #[cfg(feature = "acp-mcp")]
    #[test]
    fn hung_eviction_revokes_the_exact_facade_token_and_forgets_it() {
        let reg = Arc::new(CountingRegistrar::default());
        let registrar: Arc<dyn crate::acp_mcp::SessionTokenRegistrar> = reg.clone();
        let mut state = empty_pool_state();
        state.facade_tokens.insert("discord:acp_x".into(), "T_hung".into());
        // A different session's token must be untouched.
        state.facade_tokens.insert("discord:acp_y".into(), "T_other".into());

        super::revoke_facade_token_for_key(&mut state, "discord:acp_x", Some(&registrar));

        assert_eq!(reg.revoked(), vec!["T_hung"], "only the evicted session's token is revoked");
        assert!(!state.facade_tokens.contains_key("discord:acp_x"), "and it is forgotten");
        assert_eq!(
            state.facade_tokens.get("discord:acp_y").map(String::as_str),
            Some("T_other"),
            "an unrelated session's token is untouched"
        );
    }

    /// A failed facade config write must not mint a token. The agent has no `openab` entry, so it
    /// can never present one; minting anyway would leave a live credential registered for a
    /// session that cannot use it until eviction.
    #[cfg(feature = "acp-mcp")]
    #[tokio::test]
    async fn no_token_is_minted_when_the_facade_config_write_fails() {
        let dir = tempfile::tempdir().unwrap();
        // Make `<workdir>/.openab` a FILE, so `create_dir_all` inside the writer fails.
        //
        // This used to block on `.cursor`, which openab no longer creates: since D-15 it authors
        // only `.openab/mcp-facade.json` and never touches a vendor directory. Left pointing at
        // `.cursor` the write would SUCCEED, the test would fail, and — worse if it had been
        // written the other way round — a test asserting "no mint on failure" would have been
        // passing against a call that never failed.
        std::fs::write(dir.path().join(".openab"), b"not a directory").unwrap();

        let counting = Arc::new(CountingRegistrar::default());
        let registrar: Arc<dyn crate::acp_mcp::SessionTokenRegistrar> = counting.clone();
        let token = super::setup_facade_session(
            dir.path().to_str().unwrap(),
            "http://127.0.0.1:8848/mcp",
            "acp_x",
            &registrar,
        )
        .await;

        assert!(token.is_none(), "a failed config write must yield no token");
        assert!(
            counting.minted.lock().unwrap().is_empty(),
            "the registrar must never be asked to mint when the config could not be written"
        );
    }

    /// The happy path still mints exactly once, for the right channel.
    #[cfg(feature = "acp-mcp")]
    #[tokio::test]
    async fn a_successful_facade_config_write_mints_one_token() {
        let dir = tempfile::tempdir().unwrap();
        let counting = Arc::new(CountingRegistrar::default());
        let registrar: Arc<dyn crate::acp_mcp::SessionTokenRegistrar> = counting.clone();
        let token = super::setup_facade_session(
            dir.path().to_str().unwrap(),
            "http://127.0.0.1:8848/mcp",
            "acp_x",
            &registrar,
        )
        .await;

        assert_eq!(token.as_deref(), Some("token-xyz"));
        assert_eq!(counting.minted.lock().unwrap().as_slice(), ["acp_x"]);
    }

    #[test]
    fn remove_if_same_handle_removes_matching_entry() {
        let expected = Arc::new(Mutex::new(1_u8));
        let mut map = HashMap::from([("thread".to_string(), Arc::clone(&expected))]);

        let removed = remove_if_same_handle(&mut map, "thread", &expected);

        assert!(removed.is_some());
        assert!(map.is_empty());
    }

    #[test]
    fn remove_if_same_handle_keeps_replaced_entry() {
        let stale = Arc::new(Mutex::new(1_u8));
        let fresh = Arc::new(Mutex::new(2_u8));
        let mut map = HashMap::from([("thread".to_string(), Arc::clone(&fresh))]);

        let removed = remove_if_same_handle(&mut map, "thread", &stale);

        assert!(removed.is_none());
        let current = map.get("thread").expect("entry should remain");
        assert!(Arc::ptr_eq(current, &fresh));
    }

    #[test]
    fn get_or_insert_gate_reuses_gate_for_same_thread() {
        let mut map = HashMap::new();

        let first = get_or_insert_gate(&mut map, "thread");
        let second = get_or_insert_gate(&mut map, "thread");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn classify_idle_marks_stale_by_time() {
        let now = Instant::now();
        let cutoff = now - std::time::Duration::from_secs(60);
        let last_active = now - std::time::Duration::from_secs(120);
        assert!(classify_idle(last_active, true, cutoff));
    }

    #[test]
    fn classify_idle_marks_stale_by_death() {
        let now = Instant::now();
        let cutoff = now - std::time::Duration::from_secs(60);
        assert!(classify_idle(now, false, cutoff));
    }

    #[test]
    fn classify_idle_keeps_fresh_alive_sessions() {
        let now = Instant::now();
        let cutoff = now - std::time::Duration::from_secs(60);
        assert!(!classify_idle(now, true, cutoff));
    }

    #[test]
    fn better_candidate_prefers_empty_current() {
        assert!(better_candidate(None, Instant::now()));
    }

    #[test]
    fn better_candidate_prefers_older_last_active() {
        let older = Instant::now() - std::time::Duration::from_secs(120);
        let newer = Instant::now() - std::time::Duration::from_secs(30);
        assert!(better_candidate(Some(newer), older));
    }

    #[test]
    fn better_candidate_rejects_newer_last_active() {
        let older = Instant::now() - std::time::Duration::from_secs(120);
        let newer = Instant::now() - std::time::Duration::from_secs(30);
        assert!(!better_candidate(Some(older), newer));
    }

    #[test]
    fn classify_hung_detects_in_flight_session_past_threshold() {
        assert!(classify_hung(
            true,
            std::time::Duration::from_secs(200),
            std::time::Duration::from_secs(120),
        ));
    }

    #[test]
    fn classify_hung_ignores_in_flight_session_within_threshold() {
        assert!(!classify_hung(
            true,
            std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(120),
        ));
    }

    #[test]
    fn classify_hung_never_marks_idle_sessions() {
        assert!(!classify_hung(
            false,
            std::time::Duration::from_secs(200),
            std::time::Duration::from_secs(120),
        ));
    }

    #[test]
    fn better_candidate_keeps_existing_on_equal_last_active() {
        let ts = Instant::now() - std::time::Duration::from_secs(60);
        assert!(!better_candidate(Some(ts), ts));
    }

    /// The force-evict warning must log NEITHER id raw — both the `acp_<uuid>` channel (inside the
    /// `<platform>:<channel_id>` pool key) and the `sess_<uuid>` session id resume the session. A
    /// capture subscriber exercises the real `warn!` macro, so a revert to raw fields fails here
    /// rather than silently shipping a credential to the logs (F6 / round 6).
    #[test]
    fn force_evict_warning_redacts_both_ids() {
        use std::io::Write;
        use std::sync::{Arc as StdArc, Mutex as StdMutex};

        #[derive(Clone)]
        struct Cap(StdArc<StdMutex<Vec<u8>>>);
        impl Write for Cap {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let uuid = "00000000-0000-0000-0000-000000000000";
        let buf = StdArc::new(StdMutex::new(Vec::new()));
        let cap = Cap(buf.clone());
        let sub = tracing_subscriber::fmt()
            .with_writer(move || cap.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(sub, || {
            super::warn_force_evicting_hung(
                &format!("discord:acp_{uuid}"),
                Some(&format!("sess_{uuid}")),
                999,
                600,
            );
        });

        let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(out.contains("force-evicting hung session"), "the warning must fire: {out}");
        assert!(!out.contains(uuid), "no raw uuid may reach the log: {out}");
        assert!(!out.contains("acp_") && !out.contains("sess_"), "no raw id prefix either: {out}");
        assert!(out.contains('#'), "the redaction tag must be present: {out}");
        assert!(out.contains("discord"), "the readable platform half must survive: {out}");
    }

    #[test]
    fn purge_session_entries_drops_all_entries_for_evicted_key_only() {
        let mut state = PoolState {
            active: HashMap::new(),
            cancel_handles: HashMap::new(),
            #[cfg(feature = "acp-mcp")]
            facade_tokens: HashMap::new(),
            activity: HashMap::from([
                ("hung".to_string(), Arc::new(SessionActivity::new())),
                ("other".to_string(), Arc::new(SessionActivity::new())),
            ]),
            pgids: HashMap::from([("hung".to_string(), 1234), ("other".to_string(), 5678)]),
            suspended: HashMap::from([
                ("hung".to_string(), "session-hung".to_string()),
                ("other".to_string(), "session-other".to_string()),
            ]),
            persisted: HashMap::from([
                ("hung".to_string(), "session-hung".to_string()),
                ("other".to_string(), "session-other".to_string()),
            ]),
            creating: HashMap::from([("hung".to_string(), Arc::new(Mutex::new(())))]),
            session_workdirs: HashMap::from([("hung".to_string(), "/tmp/ws".to_string())]),
            session_projects: HashMap::new(),
        };

        purge_session_entries(&mut state, "hung");

        // Evicted key must not be resumable: no suspended/persisted entry left.
        assert!(!state.activity.contains_key("hung"));
        assert!(!state.cancel_handles.contains_key("hung"));
        assert!(!state.pgids.contains_key("hung"));
        assert!(!state.suspended.contains_key("hung"));
        assert!(!state.persisted.contains_key("hung"));
        assert!(!state.session_workdirs.contains_key("hung"));
        assert!(!state.session_projects.contains_key("hung"));
        // The creating gate is concurrency control, not session state: it must
        // survive so an in-flight get_or_create holder stays serialized.
        assert!(state.creating.contains_key("hung"));
        assert_eq!(state.pgids.get("other"), Some(&5678));
        // Other keys survive untouched.
        assert_eq!(
            state.persisted.get("other"),
            Some(&"session-other".to_string())
        );
        assert_eq!(
            state.suspended.get("other"),
            Some(&"session-other".to_string())
        );
        assert!(state.activity.contains_key("other"));
    }

    #[test]
    fn persisted_mapping_can_include_active_and_suspended_sessions() {
        let persisted = HashMap::from([
            ("active-thread".to_string(), "session-active".to_string()),
            (
                "suspended-thread".to_string(),
                "session-suspended".to_string(),
            ),
        ]);

        let serialized =
            serde_json::to_string_pretty(&persisted).expect("serialize persisted mapping");
        let roundtrip: HashMap<String, String> =
            serde_json::from_str(&serialized).expect("deserialize persisted mapping");

        assert_eq!(
            roundtrip.get("active-thread"),
            Some(&"session-active".to_string())
        );
        assert_eq!(
            roundtrip.get("suspended-thread"),
            Some(&"session-suspended".to_string())
        );
    }

    // --- Project-context binding tests (workflow 20260818-openab-project-scoped-acp-session-bootstrap) ---

    use super::{canonicalize_pinned, resolve_effective_workdir};

    /// Helper: run the production canonicalize_pinned + resolve_effective_workdir
    /// pair, the same path `get_or_create` takes. Lets helper tests exercise
    /// the contract as it actually runs.
    fn resolve_with_canonical(
        project: Option<&ProjectContext>,
        stored_workdir: Option<&str>,
        config_workdir: &str,
    ) -> (String, Option<ProjectContext>) {
        let canonical = canonicalize_pinned(project).expect("canonicalize pinned");
        resolve_effective_workdir(project, canonical.as_ref(), stored_workdir, config_workdir)
    }

    /// No project context, no stored binding → falls back to config.working_dir
    /// (req #A, req #G).
    #[test]
    fn resolve_falls_back_to_config_when_no_project_and_no_stored() {
        let (wd, store) = resolve_with_canonical(None, None, "/cfg/work");
        assert_eq!(wd, "/cfg/work");
        assert!(store.is_none(), "nothing to persist when there is no context");
    }

    /// Stored binding wins when no project context is supplied
    /// (legacy immutability, ADR §4.5).
    #[test]
    fn resolve_prefers_stored_when_no_project() {
        let (wd, store) = resolve_with_canonical(None, Some("/stored/ws"), "/cfg/work");
        assert_eq!(wd, "/stored/ws", "stored binding wins over config");
        assert!(store.is_none());
    }

    /// Project-pinned context is authoritative — its project_root overrides
    /// both stored and config (req #B, req #6).
    #[test]
    fn resolve_prefers_project_root_when_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let project = ProjectContext {
            project_id: "openab".into(),
            project_root: dir.path().to_path_buf(),
        };
        let (wd, store) = resolve_with_canonical(Some(&project), Some("/stored/ws"), "/cfg/work");
        // The workdir is the canonical project_root, not the stored hint.
        let expected = dir.path().canonicalize().unwrap();
        assert_eq!(wd, expected.to_string_lossy());
        let store = store.expect("project-pinned context must persist");
        assert_eq!(store.project_id, "openab");
        assert_eq!(store.project_root, expected);
    }

    /// Anonymous contexts preserve the legacy `stored > override > config`
    /// precedence so `[[ws:@alias]]` directives don't drift a thread's
    /// workspace (req #3).
    #[test]
    fn resolve_anonymous_prefers_stored_over_anonymous_path() {
        let dir = tempfile::tempdir().unwrap();
        let anonymous = ProjectContext::anonymous(dir.path().to_path_buf());
        let (wd, store) =
            resolve_with_canonical(Some(&anonymous), Some("/stored/ws"), "/cfg/work");
        assert_eq!(
            wd, "/stored/ws",
            "anonymous context must defer to stored binding (ADR §4.5 immutability)"
        );
        assert!(
            store.is_none(),
            "anonymous contexts must NOT persist a project binding"
        );
    }

    /// Anonymous contexts with no stored binding use the anonymous path
    /// directly. The path is trusted as caller-validated (resolve_workspace
    /// does so upstream).
    #[test]
    fn resolve_anonymous_uses_anonymous_path_when_no_stored() {
        let dir = tempfile::tempdir().unwrap();
        let anonymous = ProjectContext::anonymous(dir.path().to_path_buf());
        let (wd, store) = resolve_with_canonical(Some(&anonymous), None, "/cfg/work");
        assert_eq!(wd, dir.path().to_string_lossy().to_string());
        assert!(store.is_none());
    }

    /// Project-pinned contexts validate `project_root` at canonicalize time.
    /// A nonexistent path fails closed (req #E).
    #[test]
    fn canonicalize_rejects_nonexistent_project_root() {
        let project = ProjectContext {
            project_id: "openab".into(),
            project_root: PathBuf::from("/nonexistent/path/2026_08_18"),
        };
        let err =
            canonicalize_pinned(Some(&project)).expect_err("nonexistent root must fail");
        assert!(err.contains("cannot be canonicalized"), "{err}");
    }

    /// Project-pinned contexts validate that `project_root` is a directory
    /// (req #E).
    #[test]
    fn canonicalize_rejects_file_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file.txt");
        std::fs::write(&file, b"x").unwrap();
        let project = ProjectContext {
            project_id: "openab".into(),
            project_root: file.clone(),
        };
        let err = canonicalize_pinned(Some(&project)).expect_err("file root must fail");
        assert!(err.contains("not a directory"), "{err}");
    }

    /// `PoolState.session_projects` round-trips through serde_json: the
    /// persistence path used by `save_projects` / `load_projects` is
    /// faithful across a write→read cycle (req #F).
    #[test]
    fn session_projects_persists_across_save_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        let projects = HashMap::from([(
            "discord:thread-1".to_string(),
            ProjectContext {
                project_id: "openab".into(),
                project_root: canonical.clone(),
            },
        )]);
        let serialized = serde_json::to_string_pretty(&projects).expect("serialize");
        let roundtrip: HashMap<String, ProjectContext> =
            serde_json::from_str(&serialized).expect("deserialize");

        let stored = roundtrip
            .get("discord:thread-1")
            .expect("thread-1 binding survives roundtrip");
        assert_eq!(stored.project_id, "openab");
        assert_eq!(stored.project_root, canonical);
    }

    /// `load_projects` returns an empty map for missing files (startup
    /// must never fail because no persistence file exists yet).
    #[test]
    fn load_projects_returns_empty_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session_projects.json");
        let projects = SessionPool::load_projects(&path).expect("missing file is Ok(empty)");
        assert!(projects.is_empty());
    }

    /// `load_projects` reports corrupt JSON via `Err` so the pool can set
    /// `projects_corrupt = true` and fail closed. The previous
    /// "corrupt → empty" behavior is what Defect 4 of workflow
    /// 20260818-openab-project-session-pinning-hardening corrected.
    #[test]
    fn load_projects_reports_corrupt_file_as_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session_projects.json");
        std::fs::write(&path, b"{ this is not valid json").unwrap();
        let result = SessionPool::load_projects(&path);
        assert!(
            result.is_err(),
            "a corrupt persistence file MUST report Err so the pool can fail closed"
        );
    }

    /// Project bindings survive a `purge_session_entries` call (req #F,
    /// matches `purge_session_entries_drops_all_entries_for_evicted_key_only`).
    #[test]
    fn purge_session_entries_clears_project_binding() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = PoolState {
            active: HashMap::new(),
            cancel_handles: HashMap::new(),
            #[cfg(feature = "acp-mcp")]
            facade_tokens: HashMap::new(),
            activity: HashMap::new(),
            pgids: HashMap::new(),
            suspended: HashMap::new(),
            persisted: HashMap::new(),
            creating: HashMap::new(),
            session_workdirs: HashMap::new(),
            session_projects: HashMap::from([(
                "evicted".to_string(),
                ProjectContext {
                    project_id: "openab".into(),
                    project_root: dir.path().to_path_buf(),
                },
            )]),
        };

        purge_session_entries(&mut state, "evicted");

        assert!(
            !state.session_projects.contains_key("evicted"),
            "project binding must be cleared alongside other session state"
        );
    }

    // --- Direct `get_or_create` regression tests (VERIFIER correction) ---
    //
    // These tests drive `SessionPool::get_or_create` end-to-end against a
    // tiny ACP-compatible shell-script agent so the actual session-creation
    // seam is exercised — not just the pure `resolve_effective_workdir`
    // helper. The mismatch gate (pool.rs:608-636) fires BEFORE
    // `AcpConnection::spawn`, so test 1 never reaches the subprocess; tests 2
    // and 3 do reach it and depend on `TEST_AGENT_SCRIPT` responding to
    // `initialize` / `session/new` / `session/load` with valid JSON-RPC.
    //
    // Unix-only: the agent script uses `/bin/sh` and POSIX process spawning.
    // `AcpConnection::spawn` itself is cross-platform, but the test fixture
    // here is not; gating on `cfg(unix)` matches the existing pattern in
    // `AcpConnection::spawn` and the rest of the codebase.

    /// Minimal ACP-compatible test agent. Reads JSON-RPC lines from stdin
    /// and writes back valid responses for `initialize`, `session/new`, and
    /// `session/load`. Any other method gets a generic success reply so the
    /// reader loop never hangs waiting for an id we will not see.
    ///
    /// When invoked as `test-acp-agent.sh <record_file>`, every received
    /// JSON-RPC line is appended to `record_file` (after the file is
    /// truncated on start) so tests can assert on the actual params the
    /// openab pool sent — most importantly `session/new.params.cwd`.
    /// Workflow 20260818-openab-project-session-pinning-hardening Defect 3
    /// requires direct verification of the cwd payload, not inference from
    /// `SessionPool` state.
    const TEST_AGENT_SCRIPT: &str = r#"#!/bin/sh
# Minimal ACP-compatible test agent.
# Usage: test-acp-agent.sh [record_file]
#   When `record_file` is set, every received JSON-RPC line is appended to
#   it (truncated on start) so tests can assert on the actual params the
#   openab pool sent. The cwd in session/new.params is the canonical
#   project_root for project-pinned calls and the configured working_dir
#   for legacy no-project calls.
RECORD="${1:-}"
if [ -n "$RECORD" ]; then
    : > "$RECORD"  # truncate on start
fi
while IFS= read -r line; do
    if [ -n "$RECORD" ]; then
        printf '%s\n' "$line" >> "$RECORD"
    fi
    case "$line" in
        *initialize*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"test"},"agentCapabilities":{"loadSession":true}}}' ;;
        *session/new*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess_test"}}' ;;
        *session/load*) printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"sessionId":"sess_test"}}' ;;
        *) printf '%s\n' '{"jsonrpc":"2.0","id":0,"result":{}}' ;;
    esac
done
"#;

    /// Write `TEST_AGENT_SCRIPT` to `dir` as an executable and return the
    /// path. Caller passes it as `AgentConfig.command`.
    #[cfg(unix)]
    fn write_test_agent_script(dir: &std::path::Path) -> PathBuf {
        let script = dir.join("test-acp-agent.sh");
        std::fs::write(&script, TEST_AGENT_SCRIPT).expect("write test agent script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod test agent script");
        script
    }

    /// Convenience: read the recorded JSON-RPC lines for a test agent that
    /// was spawned with the record file as its first arg. Returns the raw
    /// lines (one per stdin line received by the agent).
    #[cfg(unix)]
    fn read_recorded_lines(path: &std::path::Path) -> Vec<String> {
        let raw = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("read record file {}: {e}", path.display()));
        raw.lines()
            .map(std::string::ToString::to_string)
            .collect()
    }

    /// Convenience: extract `cwd` from the first `session/new` line in a
    /// record file. Panics with a clear message if no `session/new` line
    /// was found or the JSON could not be parsed.
    #[cfg(unix)]
    fn cwd_from_session_new(record_path: &std::path::Path) -> String {
        let lines = read_recorded_lines(record_path);
        for line in &lines {
            if line.contains("session/new") {
                let v: serde_json::Value = serde_json::from_str(line)
                    .unwrap_or_else(|e| panic!("parse session/new line {line}: {e}"));
                let cwd = v
                    .get("params")
                    .and_then(|p| p.get("cwd"))
                    .and_then(|c| c.as_str())
                    .unwrap_or_else(|| panic!("session/new line missing cwd: {line}"));
                return cwd.to_string();
            }
        }
        panic!(
            "no session/new line found in record file {}: lines were {:?}",
            record_path.display(),
            lines
        );
    }

    /// Test 1 — `get_or_create` rejects a project-context mismatch BEFORE
    /// reusing or spawning the existing session (req #5, req #D).
    ///
    /// The mismatch check at `pool.rs:608-636` fires before
    /// `AcpConnection::spawn`, so we don't need a working agent command for
    /// this test — `/bin/true` is enough. The key assertion is that the
    /// existing binding survives intact.
    #[tokio::test]
    async fn get_or_create_rejects_project_context_mismatch_on_existing_binding() {
        let dir = tempfile::tempdir().unwrap();
        let project_a_dir = tempfile::tempdir().unwrap();
        let project_b_dir = tempfile::tempdir().unwrap();

        let project_a = ProjectContext {
            project_id: "A".into(),
            project_root: project_a_dir.path().to_path_buf(),
        };
        let project_b = ProjectContext {
            project_id: "B".into(),
            project_root: project_b_dir.path().to_path_buf(),
        };

        let state = PoolState {
            active: HashMap::new(),
            cancel_handles: HashMap::new(),
            #[cfg(feature = "acp-mcp")]
            facade_tokens: HashMap::new(),
            activity: HashMap::new(),
            pgids: HashMap::new(),
            suspended: HashMap::new(),
            persisted: HashMap::new(),
            creating: HashMap::new(),
            session_workdirs: HashMap::new(),
            session_projects: HashMap::from([("T".to_string(), project_a.clone())]),
        };

        let pool = SessionPool::with_state_for_test(
            AgentConfig {
                // Doesn't matter — mismatch fires before spawn.
                command: "/bin/true".into(),
                args: vec![],
                working_dir: "/tmp".into(),
                env: HashMap::new(),
                inherit_env: vec![],
                command_explicit: true,
            },
            state,
            dir.path().join("session_projects.json"),
        );

        let err = pool
            .get_or_create("T", Some(&project_b))
            .await
            .expect_err("project B must be rejected when T is bound to project A");
        let msg = err.to_string();
        assert!(
            msg.contains("project context mismatch"),
            "error must mention project context mismatch: {msg}"
        );

        // The existing binding for T must remain intact (the rejected call
        // must NOT have erased or replaced it).
        let state = pool.state.read().await;
        let stored = state
            .session_projects
            .get("T")
            .expect("T's project A binding must survive the rejected call");
        assert_eq!(stored.project_id, "A");
        assert_eq!(
            stored.project_root,
            project_a_dir.path().canonicalize().unwrap()
        );
        // And no connection was created for T.
        assert!(
            state.active.get("T").is_none(),
            "no connection must exist after a rejected mismatch call"
        );
    }

    /// Test 2 — Two threads with different project contexts remain isolated.
    /// T1 is pre-bound to project A; T2 receives project B via get_or_create.
    /// After both operations, neither binding overwrites the other (req #C).
    #[cfg(unix)]
    #[tokio::test]
    async fn get_or_create_keeps_two_threads_with_different_projects_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let project_a_dir = tempfile::tempdir().unwrap();
        let project_b_dir = tempfile::tempdir().unwrap();

        let project_a = ProjectContext {
            project_id: "A".into(),
            project_root: project_a_dir.path().to_path_buf(),
        };
        let project_b = ProjectContext {
            project_id: "B".into(),
            project_root: project_b_dir.path().to_path_buf(),
        };

        let agent_script = write_test_agent_script(dir.path());

        let state = PoolState {
            active: HashMap::new(),
            cancel_handles: HashMap::new(),
            #[cfg(feature = "acp-mcp")]
            facade_tokens: HashMap::new(),
            activity: HashMap::new(),
            pgids: HashMap::new(),
            suspended: HashMap::new(),
            persisted: HashMap::new(),
            creating: HashMap::new(),
            session_workdirs: HashMap::new(),
            session_projects: HashMap::from([("T1".to_string(), project_a.clone())]),
        };

        let pool = SessionPool::with_state_for_test(
            AgentConfig {
                command: agent_script.to_string_lossy().into(),
                args: vec![],
                working_dir: dir.path().to_string_lossy().into(),
                env: HashMap::new(),
                inherit_env: vec![],
                command_explicit: true,
            },
            state,
            dir.path().join("session_projects.json"),
        );

        // T2 receives project B via get_or_create. This must succeed and
        // create a fresh binding without disturbing T1's pre-existing
        // binding. The agent script responds to initialize/session/new so
        // the spawn path completes normally.
        let created = pool
            .get_or_create("T2", Some(&project_b))
            .await
            .expect("T2 spawn must succeed against the test agent");
        assert!(created, "T2 must be a fresh session");

        let state = pool.state.read().await;

        // T1's binding is untouched.
        let t1 = state
            .session_projects
            .get("T1")
            .expect("T1's pre-existing project A binding must survive");
        assert_eq!(t1.project_id, "A");
        assert_eq!(
            t1.project_root,
            project_a_dir.path().canonicalize().unwrap()
        );

        // T2 has a fresh project B binding.
        let t2 = state
            .session_projects
            .get("T2")
            .expect("T2 must have its own project B binding");
        assert_eq!(t2.project_id, "B");
        assert_eq!(
            t2.project_root,
            project_b_dir.path().canonicalize().unwrap()
        );

        // T2's session_workdirs entry must reflect the project B root, and
        // must not collide with T1's entry (T1 had no workdir pre-existing
        // because its binding was injected directly via the test seam —
        // only T2 goes through the spawn path here).
        let t2_workdir = state
            .session_workdirs
            .get("T2")
            .expect("T2's workdir must be recorded after spawn");
        assert_eq!(
            *t2_workdir,
            project_b_dir.path().canonicalize().unwrap().to_string_lossy().to_string(),
            "T2's workdir must reflect the dynamic project B root"
        );
    }

    /// Test 3 — Dynamic `project_root` becomes the effective workdir used
    /// by `get_or_create` and is persisted to `session_workdirs[T]` after
    /// successful creation (req #B, NOT just the helper).
    ///
    /// The test deliberately sets `config.working_dir` to a path that is
    /// NEITHER project_root NOR config_root; if the dynamic project_root
    /// actually drives the spawn, the persisted workdir must be the
    /// canonical project_root, never the config value.
    #[cfg(unix)]
    #[tokio::test]
    async fn get_or_create_uses_project_root_as_effective_workdir() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let config_root = tempfile::tempdir().unwrap();

        let agent_script = write_test_agent_script(dir.path());

        let pool = SessionPool::with_state_for_test(
            AgentConfig {
                command: agent_script.to_string_lossy().into(),
                args: vec![],
                working_dir: config_root.path().to_string_lossy().into(),
                env: HashMap::new(),
                inherit_env: vec![],
                command_explicit: true,
            },
            PoolState {
                active: HashMap::new(),
                cancel_handles: HashMap::new(),
                #[cfg(feature = "acp-mcp")]
                facade_tokens: HashMap::new(),
                activity: HashMap::new(),
                pgids: HashMap::new(),
                suspended: HashMap::new(),
                persisted: HashMap::new(),
                creating: HashMap::new(),
                session_workdirs: HashMap::new(),
                session_projects: HashMap::new(),
            },
            dir.path().join("session_projects.json"),
        );

        let project = ProjectContext {
            project_id: "openab".into(),
            project_root: project_dir.path().to_path_buf(),
        };

        let created = pool
            .get_or_create("T", Some(&project))
            .await
            .expect("spawn against the test agent must succeed");
        assert!(created, "T must be a fresh session");

        let state = pool.state.read().await;

        // The project_root must be the canonical path actually used.
        let expected = project_dir.path().canonicalize().unwrap();

        // session_projects[T] records the canonical project context.
        let stored = state
            .session_projects
            .get("T")
            .expect("T must have a project binding after get_or_create");
        assert_eq!(stored.project_id, "openab");
        assert_eq!(stored.project_root, expected);

        // session_workdirs[T] records the canonical project_root — proving
        // the dynamic project_root, NOT the config.working_dir, drove the
        // spawn and was persisted as the effective workdir.
        let stored_workdir = state
            .session_workdirs
            .get("T")
            .expect("T must have a session_workdirs entry after get_or_create");
        assert_eq!(*stored_workdir, expected.to_string_lossy().to_string());
        assert_ne!(
            *stored_workdir,
            config_root.path().canonicalize().unwrap().to_string_lossy().to_string(),
            "config.working_dir must NOT win over the dynamic project_root"
        );
    }

    // ───────────────────────────────────────────────────────────────────────
    //  Required regression tests for workflow
    //  20260818-openab-project-session-pinning-hardening
    // ───────────────────────────────────────────────────────────────────────
    //
    //  These exercise the production `SessionPool::get_or_create` end-to-end
    //  against the test agent and use the `cwd` capture seam (Defect 3) to
    //  verify what was actually sent to `session/new`. Each test maps to one
    //  required case in the workflow's REQUIRED TESTS section.

    /// Build a pool whose agent command will record the JSON-RPC lines it
    /// receives into `record_path`. Returns the pool plus a clone of the
    /// AgentConfig (so callers can use the same config-root value when
    /// asserting on legacy behavior).
    #[cfg(unix)]
    fn build_recording_pool(
        dir: &std::path::Path,
        config_working_dir: &std::path::Path,
        state: PoolState,
    ) -> (SessionPool, PathBuf) {
        let record_path = dir.join("recorded-rpc.log");
        let agent_script = write_test_agent_script(dir);
        let pool = SessionPool::with_state_for_test(
            AgentConfig {
                command: agent_script.to_string_lossy().into(),
                // Pass the record path as the agent's $1; the script writes
                // every received line to it.
                args: vec![record_path.to_string_lossy().into_owned()],
                working_dir: config_working_dir.to_string_lossy().into(),
                env: HashMap::new(),
                inherit_env: vec![],
                command_explicit: true,
            },
            state,
            dir.join("session_projects.json"),
        );
        (pool, record_path)
    }

    // ── REQUIRED TEST 1 ────────────────────────────────────────────────────
    //
    // active_project_a_rejects_project_b_before_alive_fast_path
    //
    // Spawns a REAL active ACP test session pinned to project A, then calls
    // get_or_create(same_thread, project_B) and asserts the mismatch gate
    // fires BEFORE the busy/alive fast paths. The previous workflow only
    // pre-populated session_projects (no live connection), so the live
    // fast path was never exercised.

    #[cfg(unix)]
    #[tokio::test]
    async fn active_project_a_rejects_project_b_before_alive_fast_path() {
        let dir = tempfile::tempdir().unwrap();
        let project_a_dir = tempfile::tempdir().unwrap();
        let project_b_dir = tempfile::tempdir().unwrap();

        let project_a = ProjectContext {
            project_id: "A".into(),
            project_root: project_a_dir.path().to_path_buf(),
        };
        let project_b = ProjectContext {
            project_id: "B".into(),
            project_root: project_b_dir.path().to_path_buf(),
        };

        // Spawn a REAL active session for T pinned to project A.
        let (pool, _record) = build_recording_pool(
            dir.path(),
            project_a_dir.path(),
            PoolState {
                active: HashMap::new(),
                cancel_handles: HashMap::new(),
                #[cfg(feature = "acp-mcp")]
                facade_tokens: HashMap::new(),
                activity: HashMap::new(),
                pgids: HashMap::new(),
                suspended: HashMap::new(),
                persisted: HashMap::new(),
                creating: HashMap::new(),
                session_workdirs: HashMap::new(),
                session_projects: HashMap::new(),
            },
        );

        let created = pool
            .get_or_create("T", Some(&project_a))
            .await
            .expect("first call must succeed against the test agent");
        assert!(created, "T's first call must be a fresh session");

        // Capture the live connection Arc for T so we can prove it is
        // preserved by the rejected second call.
        let live_arc_before = {
            let state = pool.state.read().await;
            state
                .active
                .get("T")
                .cloned()
                .expect("T must have an active session after the first call")
        };

        // Now call get_or_create with the SAME thread but a DIFFERENT
        // project. The mismatch gate must fire BEFORE any reuse, return,
        // resume, or alive fast path.
        let err = pool
            .get_or_create("T", Some(&project_b))
            .await
            .expect_err("project B must be rejected when T is alive on project A");
        let msg = err.to_string();
        assert!(
            msg.contains("project context mismatch"),
            "error must mention project context mismatch: {msg}"
        );
        assert!(
            msg.contains("project_id=\"A\""),
            "error must name the stored project A: {msg}"
        );
        assert!(
            msg.contains("project_id=\"B\""),
            "error must name the incoming project B: {msg}"
        );

        // The existing active session for T must survive intact: the same
        // Arc, still bound to project A.
        let state = pool.state.read().await;
        let live_arc_after = state
            .active
            .get("T")
            .cloned()
            .expect("T must STILL have an active session after the rejected call");
        assert!(
            Arc::ptr_eq(&live_arc_before, &live_arc_after),
            "the live connection Arc must be unchanged (no silent re-spawn or substitution)"
        );

        // The session_projects binding must still be project A (no
        // overwriting with B's identity).
        let stored = state
            .session_projects
            .get("T")
            .expect("T's binding must survive the rejected call");
        assert_eq!(stored.project_id, "A", "stored binding must remain project A");
        assert_eq!(
            stored.project_root,
            project_a_dir.path().canonicalize().unwrap()
        );

        // session_workdirs must still point at project A's root.
        let wd = state
            .session_workdirs
            .get("T")
            .expect("T's workdir must survive the rejected call");
        assert_eq!(
            *wd,
            project_a_dir.path().canonicalize().unwrap().to_string_lossy().to_string()
        );
    }

    // ── REQUIRED TEST 2 ────────────────────────────────────────────────────
    //
    // same_canonical_project_reuses_active_session
    //
    // Pre-bound active session to project A with a canonical project_root.
    // A subsequent get_or_create with a NON-canonical spelling of the
    // SAME project_root (e.g. trailing slash, "./", or with the
    // canonicalize-induced path) must compare equal after canonicalization
    // and reuse the active session. This guards the "legitimate
    // same-project reuse" half of Defect 1 — the gate must be precise,
    // not blanket-reject all reuses.

    #[cfg(unix)]
    #[tokio::test]
    async fn same_canonical_project_reuses_active_session() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();

        // The canonical form that canonicalize() will return.
        let canonical_root = project_dir.path().canonicalize().unwrap();

        // Two non-canonical spellings of the same project_root. POSIX
        // canonicalize() collapses trailing slashes; the dot-segments and
        // a relative component are also normalized. The point is that
        // canonicalize_pinned must produce the same ProjectContext for
        // both, so the mismatch check passes and the active session is
        // reused.
        let project_with_trailing_slash = ProjectContext {
            project_id: "openab".into(),
            project_root: {
                let mut p = canonical_root.clone();
                let s = p.to_string_lossy().to_string();
                p = std::path::PathBuf::from(format!("{s}/"));
                p
            },
        };
        let project_canonical = ProjectContext {
            project_id: "openab".into(),
            project_root: canonical_root.clone(),
        };

        // Sanity: the two spellings must canonicalize to byte-equal roots,
        // otherwise the test below is meaningless.
        let c1 = project_with_trailing_slash
            .canonicalized()
            .expect("canonicalize trailing slash");
        let c2 = project_canonical
            .canonicalized()
            .expect("canonicalize canonical");
        assert_eq!(
            c1.project_root, c2.project_root,
            "test setup: both spellings must canonicalize to the same path"
        );

        // Spawn a real active session pinned to project A.
        let (pool, _record) = build_recording_pool(
            dir.path(),
            project_dir.path(),
            PoolState {
                active: HashMap::new(),
                cancel_handles: HashMap::new(),
                #[cfg(feature = "acp-mcp")]
                facade_tokens: HashMap::new(),
                activity: HashMap::new(),
                pgids: HashMap::new(),
                suspended: HashMap::new(),
                persisted: HashMap::new(),
                creating: HashMap::new(),
                session_workdirs: HashMap::new(),
                session_projects: HashMap::new(),
            },
        );
        let created = pool
            .get_or_create("T", Some(&project_canonical))
            .await
            .expect("first call must succeed");
        assert!(created, "first call must be a fresh session");

        let live_arc_before = {
            let state = pool.state.read().await;
            state
                .active
                .get("T")
                .cloned()
                .expect("T must have an active session")
        };

        // Now call with the same project under a non-canonical spelling.
        // The gate must compare canonical forms and pass; the busy/alive
        // fast path then short-circuits to Ok(false) — REUSE, not
        // rejection.
        let created_again = pool
            .get_or_create("T", Some(&project_with_trailing_slash))
            .await
            .expect("same canonical project must be accepted (not a mismatch)");
        assert!(
            !created_again,
            "same canonical project must reuse the active session, not create a new one"
        );

        let state = pool.state.read().await;
        let live_arc_after = state
            .active
            .get("T")
            .cloned()
            .expect("T must still have an active session after reuse");
        assert!(
            Arc::ptr_eq(&live_arc_before, &live_arc_after),
            "the same connection Arc must be reused, not silently substituted"
        );
    }

    // ── REQUIRED TEST 3 ────────────────────────────────────────────────────
    //
    // session_new_receives_dynamic_project_root_as_cwd
    //
    // Drives get_or_create with a project-pinned context and directly
    // asserts (from the agent-side record, NOT from pool state) that the
    // dynamic project_root reached `session/new.params.cwd`. This is the
    // Defect 3 fix: prior tests inferred cwd from session_workdirs only.

    #[cfg(unix)]
    #[tokio::test]
    async fn session_new_receives_dynamic_project_root_as_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let config_root = tempfile::tempdir().unwrap();

        let (pool, record) = build_recording_pool(
            dir.path(),
            config_root.path(),
            PoolState {
                active: HashMap::new(),
                cancel_handles: HashMap::new(),
                #[cfg(feature = "acp-mcp")]
                facade_tokens: HashMap::new(),
                activity: HashMap::new(),
                pgids: HashMap::new(),
                suspended: HashMap::new(),
                persisted: HashMap::new(),
                creating: HashMap::new(),
                session_workdirs: HashMap::new(),
                session_projects: HashMap::new(),
            },
        );

        let project = ProjectContext {
            project_id: "openab".into(),
            project_root: project_dir.path().to_path_buf(),
        };
        pool.get_or_create("T", Some(&project))
            .await
            .expect("spawn must succeed");

        // The agent script recorded every JSON-RPC line it received. Read
        // session/new.params.cwd directly from the agent's view, NOT from
        // pool state.
        let cwd = cwd_from_session_new(&record);
        let expected = project_dir.path().canonicalize().unwrap();
        assert_eq!(
            cwd,
            expected.to_string_lossy(),
            "session/new.params.cwd must be the dynamic project_root"
        );
        // Sanity: the agent saw the CANONICAL form, not a trailing-slash
        // or dot-segment variant that the pool was given.
        assert!(
            !cwd.ends_with('/') || cwd == "/",
            "session/new.params.cwd must be the canonical form (no spurious trailing slash): {cwd}"
        );
    }

    // ── REQUIRED TEST 4 ────────────────────────────────────────────────────
    //
    // legacy_session_new_receives_configured_working_dir
    //
    // When no project context is supplied, get_or_create must send the
    // configured [agent].working_dir as session/new.params.cwd. This
    // confirms the legacy single-project path still works.

    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_session_new_receives_configured_working_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config_root = tempfile::tempdir().unwrap();

        let (pool, record) = build_recording_pool(
            dir.path(),
            config_root.path(),
            PoolState {
                active: HashMap::new(),
                cancel_handles: HashMap::new(),
                #[cfg(feature = "acp-mcp")]
                facade_tokens: HashMap::new(),
                activity: HashMap::new(),
                pgids: HashMap::new(),
                suspended: HashMap::new(),
                persisted: HashMap::new(),
                creating: HashMap::new(),
                session_workdirs: HashMap::new(),
                session_projects: HashMap::new(),
            },
        );

        // No project context — pure legacy path.
        let created = pool
            .get_or_create("T", None)
            .await
            .expect("legacy spawn must succeed");
        assert!(created, "T must be a fresh session");

        // The legacy path does NOT persist `session_workdirs[T]` because
        // `working_dir_override.is_some()` is the original persistence
        // gate (workspace hints only — the configured working_dir is the
        // implicit fallback, not a "hint"). The agent-side assertion
        // below is what proves the legacy path actually used the
        // configured working_dir as the spawn cwd.

        // And the agent saw the configured working_dir as session/new.cwd.
        let cwd = cwd_from_session_new(&record);
        assert_eq!(
            cwd,
            config_root.path().canonicalize().unwrap().to_string_lossy(),
            "legacy session/new.params.cwd must be config.working_dir"
        );
    }

    // ── REQUIRED TEST 5 ────────────────────────────────────────────────────
    //
    // corrupt_project_binding_does_not_resume_untrusted_session
    //
    // With the per-key untrusted set populated (simulating a startup
    // where session_projects.json failed to deserialize), a project-pinned
    // get_or_create for a thread whose key is in the untrusted set MUST
    // fail closed rather than resume the session. The original project
    // identity was lost with the corrupt file, so reusing the persisted
    // sessionId would be an unverified cross-project reuse.
    //
    // The global `projects_corrupt: AtomicBool` that this test exercised
    // in the previous bounded cycle was removed by the second-cycle
    // correction: per-key untrusted state replaced it. This test now
    // drives the per-key seam directly.

    #[cfg(unix)]
    #[tokio::test]
    async fn corrupt_project_binding_does_not_resume_untrusted_session() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();

        let project = ProjectContext {
            project_id: "openab".into(),
            project_root: project_dir.path().to_path_buf(),
        };

        // The test seam: a thread with a persisted sessionId (the agent
        // backend's sessionId for a previous pinned session whose binding
        // is now lost), an empty session_projects, AND the per-key
        // untrusted set populated for this thread (simulating the
        // population that `SessionPool::new()` performs on a corrupt
        // session_projects.json load).
        let state = PoolState {
            active: HashMap::new(),
            cancel_handles: HashMap::new(),
            #[cfg(feature = "acp-mcp")]
            facade_tokens: HashMap::new(),
            activity: HashMap::new(),
            pgids: HashMap::new(),
            suspended: HashMap::from([("T".to_string(), "sess_untrusted".to_string())]),
            persisted: HashMap::from([("T".to_string(), "sess_untrusted".to_string())]),
            creating: HashMap::new(),
            session_workdirs: HashMap::new(),
            session_projects: HashMap::new(),
        };

        let (pool, _record) = build_recording_pool(dir.path(), project_dir.path(), state);
        pool.set_untrusted_keys_for_test(["T".to_string()]).await;

        let err = pool
            .get_or_create("T", Some(&project))
            .await
            .expect_err("must fail closed: untrusted persisted session must not be reused");
        let msg = err.to_string();
        assert!(
            msg.contains("project binding") && msg.contains("untrusted"),
            "error must name the untrusted cause: {msg}"
        );

        // The persisted sessionId must still be there (we did not silently
        // drop it; the user / caller decides whether to reset).
        let state = pool.state.read().await;
        assert_eq!(
            state.persisted.get("T").map(String::as_str),
            Some("sess_untrusted"),
            "the untrusted sessionId stays persisted until the caller resets"
        );
        // T is still in the untrusted set (a failed pinned get_or_create
        // does NOT clear the untrusted marker).
        drop(state);
        assert!(
            pool.untrusted_project_keys.read().await.contains("T"),
            "a failed pinned get_or_create must NOT clear the untrusted marker"
        );

        // After resetting and retrying, the system must recover cleanly.
        // The reset removes the persisted entry AND the untrusted
        // marker for T; the retry sees no trusted-or-untrusted mapping
        // for T and starts a fresh session.
        //
        // `reset_session` returns `Err("no session for thread T")` when
        // there is no active connection (the test seed has only
        // persisted/suspended entries, no active), but it still
        // executes `purge_session_entries` as a side effect — which is
        // what clears the persisted entry. The .ok() here ignores the
        // "no active" error so the test exercises the recovery path,
        // not the no-op path.
        let _ = pool.reset_session("T").await; // may be Err (no active); side effect clears persisted
        let created = pool
            .get_or_create("T", Some(&project))
            .await
            .expect("retry after reset must succeed");
        assert!(created, "the retry must create a fresh session");
        // Reset removed T from the untrusted set, and the new
        // session's save_projects established a trusted binding.
        assert!(
            !pool.untrusted_project_keys.read().await.contains("T"),
            "T must be removed from the untrusted set after reset+recreate"
        );
    }

    // ── REQUIRED TEST 6 ────────────────────────────────────────────────────
    //
    // two_active_threads_with_different_projects_remain_isolated
    //
    // Spawns REAL active sessions for T1 (project A) and T2 (project B),
    // then verifies each thread's binding, workdir, and active connection
    // are isolated. Neither leaks the other's identity.

    #[cfg(unix)]
    #[tokio::test]
    async fn two_active_threads_with_different_projects_remain_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let project_a_dir = tempfile::tempdir().unwrap();
        let project_b_dir = tempfile::tempdir().unwrap();

        let project_a = ProjectContext {
            project_id: "A".into(),
            project_root: project_a_dir.path().to_path_buf(),
        };
        let project_b = ProjectContext {
            project_id: "B".into(),
            project_root: project_b_dir.path().to_path_buf(),
        };

        let (pool, _record) = build_recording_pool(
            dir.path(),
            dir.path(),
            PoolState {
                active: HashMap::new(),
                cancel_handles: HashMap::new(),
                #[cfg(feature = "acp-mcp")]
                facade_tokens: HashMap::new(),
                activity: HashMap::new(),
                pgids: HashMap::new(),
                suspended: HashMap::new(),
                persisted: HashMap::new(),
                creating: HashMap::new(),
                session_workdirs: HashMap::new(),
                session_projects: HashMap::new(),
            },
        );

        let created_t1 = pool
            .get_or_create("T1", Some(&project_a))
            .await
            .expect("T1 spawn must succeed");
        assert!(created_t1);
        let created_t2 = pool
            .get_or_create("T2", Some(&project_b))
            .await
            .expect("T2 spawn must succeed");
        assert!(created_t2);

        let state = pool.state.read().await;

        // T1: project A everywhere.
        let t1_proj = state
            .session_projects
            .get("T1")
            .expect("T1 has a project binding");
        assert_eq!(t1_proj.project_id, "A");
        assert_eq!(
            t1_proj.project_root,
            project_a_dir.path().canonicalize().unwrap()
        );
        let t1_wd = state.session_workdirs.get("T1").expect("T1 has a workdir");
        assert_eq!(
            *t1_wd,
            project_a_dir.path().canonicalize().unwrap().to_string_lossy().to_string()
        );
        assert!(state.active.get("T1").is_some(), "T1 has an active connection");

        // T2: project B everywhere.
        let t2_proj = state
            .session_projects
            .get("T2")
            .expect("T2 has a project binding");
        assert_eq!(t2_proj.project_id, "B");
        assert_eq!(
            t2_proj.project_root,
            project_b_dir.path().canonicalize().unwrap()
        );
        let t2_wd = state.session_workdirs.get("T2").expect("T2 has a workdir");
        assert_eq!(
            *t2_wd,
            project_b_dir.path().canonicalize().unwrap().to_string_lossy().to_string()
        );
        assert!(state.active.get("T2").is_some(), "T2 has an active connection");

        // The two connections are distinct Arcs.
        let t1_arc = state.active.get("T1").cloned().unwrap();
        let t2_arc = state.active.get("T2").cloned().unwrap();
        assert!(
            !Arc::ptr_eq(&t1_arc, &t2_arc),
            "T1 and T2 must have distinct connection Arcs (no cross-thread reuse)"
        );
    }

    // ── REQUIRED TEST 7 ────────────────────────────────────────────────────
    //
    // cleanup_reset_followed_by_new_project_creates_correctly_rooted_new_session
    //
    // Spawn T on project A, reset it, then spawn T on project B. The new
    // session must use project B's root as session/new.params.cwd AND
    // persist project B's binding — i.e. reset properly clears A so B can
    // be pinned fresh.

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_reset_followed_by_new_project_creates_correctly_rooted_new_session() {
        let dir = tempfile::tempdir().unwrap();
        let project_a_dir = tempfile::tempdir().unwrap();
        let project_b_dir = tempfile::tempdir().unwrap();

        let project_a = ProjectContext {
            project_id: "A".into(),
            project_root: project_a_dir.path().to_path_buf(),
        };
        let project_b = ProjectContext {
            project_id: "B".into(),
            project_root: project_b_dir.path().to_path_buf(),
        };

        let (pool, record) = build_recording_pool(
            dir.path(),
            dir.path(),
            PoolState {
                active: HashMap::new(),
                cancel_handles: HashMap::new(),
                #[cfg(feature = "acp-mcp")]
                facade_tokens: HashMap::new(),
                activity: HashMap::new(),
                pgids: HashMap::new(),
                suspended: HashMap::new(),
                persisted: HashMap::new(),
                creating: HashMap::new(),
                session_workdirs: HashMap::new(),
                session_projects: HashMap::new(),
            },
        );

        // Phase 1: pin T to A.
        let created = pool
            .get_or_create("T", Some(&project_a))
            .await
            .expect("first spawn must succeed");
        assert!(created);

        // Phase 2: reset.
        pool.reset_session("T")
            .await
            .expect("reset_session must succeed");
        let state = pool.state.read().await;
        assert!(state.active.get("T").is_none(), "T's active conn must be cleared");
        assert!(
            !state.session_projects.contains_key("T"),
            "T's project binding must be cleared by reset"
        );
        drop(state);

        // Truncate the agent's record file so the post-reset spawn is the
        // only session/new line we assert on.
        std::fs::write(&record, b"").expect("truncate record");

        // Phase 3: pin T to B. Must succeed (mismatch gate must NOT trip
        // because reset cleared the A binding), and the new session must
        // use B's root as session/new.params.cwd.
        let created = pool
            .get_or_create("T", Some(&project_b))
            .await
            .expect("post-reset spawn must succeed");
        assert!(created, "post-reset spawn must be a fresh session");

        let cwd = cwd_from_session_new(&record);
        let expected_b = project_b_dir.path().canonicalize().unwrap();
        assert_eq!(
            cwd,
            expected_b.to_string_lossy(),
            "post-reset session/new.params.cwd must be the NEW project B root"
        );
        assert_ne!(
            cwd,
            project_a_dir.path().canonicalize().unwrap().to_string_lossy().to_string(),
            "post-reset cwd must NOT be the OLD project A root"
        );

        // Pool state reflects B: binding, workdir, and a new persisted
        // sessionId that is NOT the one A would have produced.
        let state = pool.state.read().await;
        let stored = state
            .session_projects
            .get("T")
            .expect("T must have a new B binding after post-reset spawn");
        assert_eq!(stored.project_id, "B");
        assert_eq!(stored.project_root, expected_b);
        let wd = state
            .session_workdirs
            .get("T")
            .expect("T must have a new workdir after post-reset spawn");
        assert_eq!(*wd, expected_b.to_string_lossy().to_string());
    }

    // ── REQUIRED TEST 8 (second-cycle bounded correction) ───────────────────
    //
    // corrupt_project_bindings_remain_per_session_untrusted_after_fresh_save
    //
    // Confirms the per-key untrusted-set design (replacing the
    // previous-cycle's global `projects_corrupt: AtomicBool`). The
    // previous design had a confirmed defect: an unrelated fresh
    // save_projects (e.g. for a new key C) would unconditionally
    // clear the global flag, leaving old untrusted keys A and B able
    // to resume without the corruption guard. The per-key design
    // ensures that saving C does NOT touch A or B's untrusted
    // markers; each is only removed by its own reset/purge path or
    // its own successful pinned-save.

    #[cfg(unix)]
    #[tokio::test]
    async fn corrupt_project_bindings_remain_per_session_untrusted_after_fresh_save() {
        let dir = tempfile::tempdir().unwrap();
        let project_a_dir = tempfile::tempdir().unwrap();
        let project_b_dir = tempfile::tempdir().unwrap();
        let project_c_dir = tempfile::tempdir().unwrap();
        let project_b2_dir = tempfile::tempdir().unwrap();

        let project_a = ProjectContext {
            project_id: "A".into(),
            project_root: project_a_dir.path().to_path_buf(),
        };
        let project_b = ProjectContext {
            project_id: "B".into(),
            project_root: project_b_dir.path().to_path_buf(),
        };
        let project_c = ProjectContext {
            project_id: "C".into(),
            project_root: project_c_dir.path().to_path_buf(),
        };
        let project_b2 = ProjectContext {
            project_id: "B".into(),
            project_root: project_b2_dir.path().to_path_buf(),
        };

        // Seed: two old keys (A, B) with persisted sessionIds (their
        // original project bindings are lost because session_projects
        // is empty). A is preserved untouched through the whole test
        // to prove the per-key design does not over-reach.
        let state = PoolState {
            active: HashMap::new(),
            cancel_handles: HashMap::new(),
            #[cfg(feature = "acp-mcp")]
            facade_tokens: HashMap::new(),
            activity: HashMap::new(),
            pgids: HashMap::new(),
            suspended: HashMap::from([
                ("A".to_string(), "sess_a_old".to_string()),
                ("B".to_string(), "sess_b_old".to_string()),
            ]),
            persisted: HashMap::from([
                ("A".to_string(), "sess_a_old".to_string()),
                ("B".to_string(), "sess_b_old".to_string()),
            ]),
            creating: HashMap::new(),
            session_workdirs: HashMap::new(),
            session_projects: HashMap::new(),
        };

        let (pool, _record) = build_recording_pool(dir.path(), dir.path(), state);
        // Simulate the population that `SessionPool::new()` does on
        // load_projects failure: A and B are untrusted because they
        // had persisted mappings whose bindings were lost.
        pool.set_untrusted_keys_for_test(["A".to_string(), "B".to_string()])
            .await;

        // ── Step 1: fresh unrelated key C creates a correctly pinned
        //            session. C was never in persisted/suspended, so it
        //            is not in the untrusted set; its first pinned
        //            get_or_create passes the per-key check.
        let created_c = pool
            .get_or_create("C", Some(&project_c))
            .await
            .expect("fresh key C must succeed — it has no persisted mapping, no untrusted marker");
        assert!(created_c, "C must be a fresh session");
        // C's save_projects established a trusted binding. C is removed
        // from the untrusted set... but C was never in it, so the
        // remove is a no-op. The KEY invariant: A and B are still
        // in the untrusted set after C's save.
        let untrusted_after_c = pool.untrusted_project_keys.read().await.clone();
        assert!(
            untrusted_after_c.contains("A"),
            "A must STILL be untrusted after C's save (A is unrelated): {untrusted_after_c:?}"
        );
        assert!(
            untrusted_after_c.contains("B"),
            "B must STILL be untrusted after C's save (B is unrelated): {untrusted_after_c:?}"
        );
        assert!(
            !untrusted_after_c.contains("C"),
            "C must NOT be in the untrusted set (fresh key, never lost binding)"
        );

        // C's session_projects entry is in place.
        let state = pool.state.read().await;
        let c_binding = state
            .session_projects
            .get("C")
            .expect("C must have a project binding after successful spawn");
        assert_eq!(c_binding.project_id, "C");
        assert_eq!(
            c_binding.project_root,
            project_c_dir.path().canonicalize().unwrap()
        );
        drop(state);

        // ── Step 2: attempt to resume old key B. B is in the
        //            untrusted set → must fail closed.
        let err = pool
            .get_or_create("B", Some(&project_b))
            .await
            .expect_err("B is untrusted; a pinned get_or_create must fail closed");
        let msg = err.to_string();
        assert!(
            msg.contains("untrusted"),
            "error must name the untrusted cause: {msg}"
        );
        // B's persisted sessionId is still there (we did not silently drop).
        let state = pool.state.read().await;
        assert_eq!(
            state.persisted.get("B").map(String::as_str),
            Some("sess_b_old"),
            "B's persisted sessionId is preserved across the failed call"
        );
        drop(state);
        // A is also still untrusted (unrelated to B's failure).
        assert!(
            pool.untrusted_project_keys.read().await.contains("A"),
            "A is unaffected by B's failed call"
        );
        assert!(
            pool.untrusted_project_keys.read().await.contains("B"),
            "B remains untrusted after the failed call"
        );

        // ── Step 3: reset/purge B. After this, B leaves the untrusted
        //            set. A is still there.
        let _ = pool.reset_session("B").await; // may be Err (no active); side effect clears persisted
        let untrusted_after_reset_b = pool.untrusted_project_keys.read().await.clone();
        assert!(
            !untrusted_after_reset_b.contains("B"),
            "B must be removed from the untrusted set after reset: {untrusted_after_reset_b:?}"
        );
        assert!(
            untrusted_after_reset_b.contains("A"),
            "A must remain untrusted — reset on B does not touch A: {untrusted_after_reset_b:?}"
        );

        // ── Step 4: recreate B with a trusted project context. Must
        //            succeed now.
        let created_b = pool
            .get_or_create("B", Some(&project_b2))
            .await
            .expect("B may now be re-pinned after reset removed its untrusted marker");
        assert!(created_b, "B must be a fresh session after re-pinning");

        // ── Step 5: A remains untrusted (the per-key design does not
        //            let a fresh save on C or a re-pin on B clear A's
        //            marker). A is the canary that proves the global
        //            flag is truly gone.
        let untrusted_final = pool.untrusted_project_keys.read().await.clone();
        assert!(
            untrusted_final.contains("A"),
            "A MUST remain untrusted — proves save_projects on C and the re-pin on B did \
             not touch A's marker: {untrusted_final:?}"
        );
        assert!(
            !untrusted_final.contains("B"),
            "B is no longer untrusted (re-pinned with a trusted binding)"
        );
        assert!(
            !untrusted_final.contains("C"),
            "C is not untrusted (fresh key from the start)"
        );

        // B has a fresh trusted binding in session_projects.
        let state = pool.state.read().await;
        let b_binding = state
            .session_projects
            .get("B")
            .expect("B must have a project binding after re-pin");
        assert_eq!(b_binding.project_id, "B");
        assert_eq!(
            b_binding.project_root,
            project_b2_dir.path().canonicalize().unwrap(),
            "B's binding must be the NEW project root, not the lost old one"
        );
        // C's binding is still there.
        let c_binding = state
            .session_projects
            .get("C")
            .expect("C's binding must persist");
        assert_eq!(c_binding.project_id, "C");
    }
}

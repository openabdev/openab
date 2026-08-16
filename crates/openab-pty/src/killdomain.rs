//! The kill domain: Tier 1 only — process group plus subreaper/pidfd, best effort.
//!
//! The ADR's kill-domain section rests on measured kernel behaviour, and the two
//! problems it names are solved separately:
//!
//! * **Attribution** — which session does a live process belong to? Only the
//!   kernel can answer this, and Tier 1 cannot ask it, so it says so.
//! * **Reaping** — collecting exited children. `PR_SET_CHILD_SUBREAPER` plus a
//!   `pidfd` per tracked child covers this.
//!
//! Ancestry is never used for attribution. The spike measured two concurrent
//! sessions each leaking a double-fork orphan whose intermediate exited before
//! any scan: **both** orphans reparented to the runtime and neither was
//! attributable to its session, while reaping of both intermediates succeeded.
//! Reaping succeeds exactly where attribution fails, so lineage is only ever
//! used to *reap*, never to decide who owns a process.
//!
//! Tier 1 is best-effort because a child that calls `setsid` or double-forks
//! leaves the process group, and nothing short of a kernel-authoritative
//! membership primitive can find it again. Such a process may outlive its
//! session until the pod or task is replaced — that is the architecture's normal
//! reclamation path, not a bug. Survivors we *can* see are audited as anomalies
//! and counted on [`KillDomain::leaked_process_count`] so the condition is
//! observable rather than silent, and every type name, doc comment, and status
//! field on this path carries the best-effort label.
//!
//! # The Tier 2 implementation was removed as unreachable
//!
//! An earlier revision of this module implemented the ADR's Tier 2 (one cgroup
//! per session, membership read from `cgroup.procs`, teardown via `cgroup.kill`,
//! plus an end-to-end startup probe that classified `EROFS`/`EACCES`/`ENOENT`).
//! None of it was reachable. Tier 2's guarantee depends on the child's pid
//! landing in its cgroup **before `execv`**, and the only spawner in this crate,
//! [`crate::session::PortablePtySpawner`], cannot run a pre-`execv` hook at all:
//! `portable-pty` owns its own pre-exec closure. That is what the deleted
//! `SpawnCapability::ProcessGroupOnly` marker recorded, and it meant tier
//! selection could never pick Tier 2 for a real PTY session — this was not a
//! feature waiting to be switched on.
//!
//! The ADR keeps Tier 2 as the demand-gated design of record. Reinstating it
//! needs the spawn primitive first: a pre-`execv` cgroup-join hook (fork +
//! `setpgid`, write the child's own pid to a pre-opened `cgroup.procs`
//! descriptor, then `execv`, allocation-free between the two), which means
//! either a `portable-pty` capable of caller-supplied pre-exec hooks or a
//! hand-rolled spawner. Until then, `kill_domain_tier = "tier2-required"` is
//! refused at startup by [`resolve_tier`] rather than silently downgraded to
//! best effort.

use crate::audit::{AuditEvent, AuditKind, AuditLogger};
use crate::config::KillDomainRequirement;
use crate::{Error, Generation, SessionName};
use serde::Serialize;
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use std::os::unix::io::RawFd;

/// Polling step while waiting for a process group to drain. Small enough that
/// teardown is not perceptibly slower than the kernel, large enough not to spin.
const TIER1_POLL_STEP: Duration = Duration::from_millis(25);
/// Bound on the post-`SIGKILL` drain wait. `SIGKILL` is not catchable, so this
/// only covers scheduling latency.
const TIER1_KILL_WAIT: Duration = Duration::from_millis(500);

/// The active containment tier. One variant, because Tier 2 is not implemented
/// (see the module docs). The name carries the guarantee because this value
/// reaches operators through logs and status output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KillDomainTier {
    /// No prerequisites, and **best-effort** teardown/TTL.
    Tier1BestEffortProcessGroup,
}

impl KillDomainTier {
    /// True when convergence is not guaranteed. Callers must surface this.
    pub fn is_best_effort(self) -> bool {
        matches!(self, Self::Tier1BestEffortProcessGroup)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Tier1BestEffortProcessGroup => "tier1-best-effort-process-group",
        }
    }

    /// One log line an operator can act on: which tier is active and what it
    /// does not promise.
    pub fn describe(self) -> String {
        format!(
            "kill domain: {self} (teardown and absolute TTL are BEST-EFFORT: a process that \
             leaves its process group may outlive its session until the pod or task is replaced)"
        )
    }
}

impl fmt::Display for KillDomainTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Select the tier from the operator requirement.
///
/// The only decision left is whether to refuse: an operator who asked for the
/// hard guarantee must be told it does not exist, never handed best effort with
/// the same name.
pub fn resolve_tier(requirement: KillDomainRequirement) -> Result<KillDomainTier, Error> {
    match requirement {
        KillDomainRequirement::Tier1Allowed => Ok(KillDomainTier::Tier1BestEffortProcessGroup),
        KillDomainRequirement::Tier2Required => Err(Error::Config(
            "kill_domain_tier = tier2-required, but Tier 2 (one cgroup per session, teardown via \
             cgroup.kill) is NOT IMPLEMENTED in Phase 1: the PTY spawner cannot join a cgroup \
             before execv, so the hard guarantee cannot be provided. Refusing to start rather \
             than serving best-effort teardown under the name of a guarantee. Use \
             kill_domain_tier = tier1 to accept best-effort teardown explicitly."
                .into(),
        )),
    }
}

/// Result of `PR_SET_CHILD_SUBREAPER`.
///
/// Not fail-closed: reaping is not a confidentiality property, and Tier 1 is
/// already labelled best-effort, so a filtered `prctl` is logged and degrades
/// reaping to direct children. (`PR_SET_DUMPABLE`, which *is* a confidentiality
/// barrier, is fail-closed — see [`crate::containment`].)
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "status", content = "detail")]
pub enum SubreaperStatus {
    Active,
    /// Linux refused, typically a seccomp filter.
    Unavailable(String),
    /// Non-Linux: there is no subreaper concept. Never reported as active.
    Unsupported,
}

/// Ask the kernel to reparent orphaned descendants to this process so they can
/// be reaped. This buys reaping only — never attribution.
pub fn establish_subreaper() -> SubreaperStatus {
    #[cfg(target_os = "linux")]
    {
        let rc = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };
        if rc == 0 {
            SubreaperStatus::Active
        } else {
            SubreaperStatus::Unavailable(io::Error::last_os_error().to_string())
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        SubreaperStatus::Unsupported
    }
}

/// Caps on tracked processes and their pidfds, per session and globally, with FD
/// headroom reserved for control and WebSocket sockets.
///
/// Hitting a cap is **fail-closed**: [`SessionKillDomain::track`] returns an
/// error and the caller kills the session rather than tracking it partially. A
/// partially tracked session is worse than no session: teardown would silently
/// miss processes it believes it covered.
#[derive(Clone, Copy, Debug)]
pub struct TrackingLimits {
    pub max_tracked_per_session: usize,
    pub max_tracked_global: usize,
    pub reserved_fd_headroom: usize,
}

impl Default for TrackingLimits {
    fn default() -> Self {
        Self {
            max_tracked_per_session: 64,
            max_tracked_global: 512,
            reserved_fd_headroom: 128,
        }
    }
}

#[derive(Debug)]
struct GlobalTracking {
    tracked: AtomicUsize,
    leaked: AtomicU64,
    limits: TrackingLimits,
    /// Tracked-pidfd ceiling derived from `RLIMIT_NOFILE` minus reserved
    /// headroom, so pidfd tracking can never starve the listener of sockets.
    fd_budget: usize,
}

impl GlobalTracking {
    fn new(limits: TrackingLimits) -> Self {
        let fd_budget = open_file_limit()
            .map(|soft| soft.saturating_sub(limits.reserved_fd_headroom))
            .unwrap_or(usize::MAX);
        Self {
            tracked: AtomicUsize::new(0),
            leaked: AtomicU64::new(0),
            limits,
            fd_budget,
        }
    }

    fn ceiling(&self) -> usize {
        self.limits.max_tracked_global.min(self.fd_budget)
    }

    fn reserve(&self) -> Result<(), Error> {
        let ceiling = self.ceiling();
        let mut current = self.tracked.load(Ordering::Relaxed);
        loop {
            if current >= ceiling {
                return Err(Error::CapacityExceeded { limit: ceiling });
            }
            match self.tracked.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(()),
                Err(observed) => current = observed,
            }
        }
    }

    fn release(&self, n: usize) {
        self.tracked.fetch_sub(n, Ordering::AcqRel);
    }
}

#[cfg(unix)]
fn open_file_limit() -> Option<usize> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
    if rc != 0 {
        return None;
    }
    usize::try_from(limit.rlim_cur).ok()
}

#[cfg(not(unix))]
fn open_file_limit() -> Option<usize> {
    None
}

/// POSIX signals used by the escalation. Deliberately not a general signal type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
impl Signal {
    fn as_raw(self) -> i32 {
        match self {
            Self::Term => libc::SIGTERM,
            Self::Kill => libc::SIGKILL,
        }
    }
}

/// The process-signalling surface Tier 1 needs, behind a trait so the
/// escalation ladder can be exercised against a simulated process table instead
/// of real processes.
pub trait ProcessSignals: Send + Sync + fmt::Debug {
    fn signal_group(&self, pgid: i32, signal: Signal) -> io::Result<()>;
    fn signal_pid(&self, pid: i32, signal: Signal) -> io::Result<()>;
    fn group_alive(&self, pgid: i32) -> bool;
    fn pid_alive(&self, pid: i32) -> bool;
    /// Non-blocking reap. `true` when the pid was collected or is already gone.
    fn reap_nonblocking(&self, pid: i32) -> bool;
}

/// The real implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemSignals;

#[cfg(unix)]
impl ProcessSignals for SystemSignals {
    fn signal_group(&self, pgid: i32, signal: Signal) -> io::Result<()> {
        // ESRCH means the group is already gone, which is success for teardown.
        let rc = unsafe { libc::kill(-pgid, signal.as_raw()) };
        if rc == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    fn signal_pid(&self, pid: i32, signal: Signal) -> io::Result<()> {
        let rc = unsafe { libc::kill(pid, signal.as_raw()) };
        if rc == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    fn group_alive(&self, pgid: i32) -> bool {
        if unsafe { libc::kill(-pgid, 0) } != 0 {
            // EPERM: the group exists but is not ours to signal. Treat as alive
            // so a survivor is reported rather than assumed gone.
            return io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        }
        // `kill(-pgid, 0)` succeeds while *any* member exists, including one that
        // has already exited. That distinction is load-bearing here, for the same
        // reason `reap_tracked` is unconditional: `PR_SET_CHILD_SUBREAPER`
        // reparents a killed session's untracked descendants to the runtime,
        // which does not wait on processes it never tracked, so their zombies
        // stay in the process group. A signal-only probe therefore reports every
        // teardown of a shell that ever forked as leaking — measured on kernel
        // 6.8, three zombies with `ppid == runtime` remained in the group and the
        // session leader's pgid was reported as a survivor on every teardown,
        // after burning the full grace and kill budget waiting for them. A
        // counter that always fires cannot surface the leaks it exists for.
        #[cfg(target_os = "linux")]
        {
            linux_group_has_live_member(pgid)
        }
        #[cfg(not(target_os = "linux"))]
        {
            true
        }
    }

    fn pid_alive(&self, pid: i32) -> bool {
        if unsafe { libc::kill(pid, 0) } == 0 {
            return true;
        }
        // EPERM means it exists but is not ours to signal; treat as alive so a
        // survivor is reported rather than assumed gone.
        io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    fn reap_nonblocking(&self, pid: i32) -> bool {
        let mut status: libc::c_int = 0;
        let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        rc == pid || rc == -1
    }
}

/// True when the process group has at least one member that has not exited.
///
/// Read from `/proc/<pid>/stat` because no signal can express the difference: a
/// zombie is still a member of its process group and still accepts `kill(pgid,
/// 0)`, but it has exited and is not a survivor.
#[cfg(target_os = "linux")]
fn linux_group_has_live_member(pgid: i32) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        // Cannot tell. Report alive rather than claim a convergence we did not
        // observe.
        return true;
    };
    for entry in entries.flatten() {
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `comm` may contain spaces and parentheses, so parse after the last ')'.
        let Some((_, tail)) = stat.rsplit_once(") ") else {
            continue;
        };
        let mut fields = tail.split_whitespace();
        let state = fields.next().unwrap_or("Z");
        let _ppid = fields.next();
        let group = fields.next().and_then(|value| value.parse::<i32>().ok());
        if group == Some(pgid) && state != "Z" {
            return true;
        }
    }
    false
}

#[cfg(not(unix))]
impl ProcessSignals for SystemSignals {
    fn signal_group(&self, _pgid: i32, _signal: Signal) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
    fn signal_pid(&self, _pid: i32, _signal: Signal) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
    fn group_alive(&self, _pgid: i32) -> bool {
        false
    }
    fn pid_alive(&self, _pid: i32) -> bool {
        false
    }
    fn reap_nonblocking(&self, _pid: i32) -> bool {
        true
    }
}

/// A process this session is tracking for reaping and defence-in-depth
/// signalling.
#[derive(Debug)]
struct TrackedChild {
    pid: i32,
    #[cfg(target_os = "linux")]
    pidfd: Option<PidFd>,
}

/// Owned `pidfd`. Signalling through it cannot hit a recycled pid, which is why
/// it is preferred over `kill(pid)` for tracked children.
#[cfg(target_os = "linux")]
#[derive(Debug)]
struct PidFd(RawFd);

#[cfg(target_os = "linux")]
impl PidFd {
    fn open(pid: i32) -> io::Result<Self> {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(fd as RawFd))
    }

    fn send_signal(&self, signal: Signal) -> io::Result<()> {
        let rc = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.0 as libc::c_long,
                signal.as_raw() as libc::c_long,
                std::ptr::null::<libc::c_void>(),
                0,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for PidFd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

/// Result of a teardown. `is_best_effort` is what distinguishes "we saw no
/// survivors" from "there can be no survivors" — and in this crate it is always
/// the former.
#[derive(Clone, Debug, Serialize)]
pub struct TeardownOutcome {
    pub tier: KillDomainTier,
    /// Processes still alive after escalation. This is only the *visible*
    /// remainder: pgid escapees the runtime never tracked are, by construction,
    /// invisible here.
    pub survivors: Vec<i32>,
    pub elapsed_ms: u64,
}

impl TeardownOutcome {
    /// True when nothing visible survived. This is still a best-effort
    /// statement — see [`TeardownOutcome::is_best_effort`].
    pub fn converged(&self) -> bool {
        self.survivors.is_empty()
    }

    pub fn is_best_effort(&self) -> bool {
        self.tier.is_best_effort()
    }
}

/// Manager-level kill domain: the tier plus global tracking budgets.
#[derive(Debug)]
pub struct KillDomain {
    tier: KillDomainTier,
    subreaper: SubreaperStatus,
    global: Arc<GlobalTracking>,
    signals: Arc<dyn ProcessSignals>,
    audit: AuditLogger,
}

/// Operator-facing status. Every field an operator could mistake for a
/// containment guarantee is labelled.
#[derive(Clone, Debug, Serialize)]
pub struct KillDomainStatus {
    pub tier: KillDomainTier,
    /// Duplicated as an explicit boolean so a status consumer cannot miss it.
    pub teardown_best_effort: bool,
    pub absolute_ttl_best_effort: bool,
    pub subreaper: SubreaperStatus,
    pub tracked_processes: usize,
    pub tracked_process_ceiling: usize,
    /// Leak metric: visible survivors observed across all teardowns.
    pub leaked_processes: u64,
}

impl KillDomain {
    pub fn new(limits: TrackingLimits, audit: AuditLogger) -> Self {
        Self::with_signals(limits, audit, Arc::new(SystemSignals))
    }

    pub fn with_signals(
        limits: TrackingLimits,
        audit: AuditLogger,
        signals: Arc<dyn ProcessSignals>,
    ) -> Self {
        Self {
            tier: KillDomainTier::Tier1BestEffortProcessGroup,
            subreaper: establish_subreaper(),
            global: Arc::new(GlobalTracking::new(limits)),
            signals,
            audit,
        }
    }

    pub fn tier(&self) -> KillDomainTier {
        self.tier
    }

    pub fn leaked_process_count(&self) -> u64 {
        self.global.leaked.load(Ordering::Relaxed)
    }

    pub fn status(&self) -> KillDomainStatus {
        let best_effort = self.tier.is_best_effort();
        KillDomainStatus {
            tier: self.tier,
            teardown_best_effort: best_effort,
            // The absolute TTL depends on teardown actually reclaiming the
            // session, so it inherits the same qualifier.
            absolute_ttl_best_effort: best_effort,
            subreaper: self.subreaper.clone(),
            tracked_processes: self.global.tracked.load(Ordering::Relaxed),
            tracked_process_ceiling: self.global.ceiling(),
            leaked_processes: self.leaked_process_count(),
        }
    }

    /// Prepare a session's kill domain.
    ///
    /// `generation` is carried only so anomaly audits name the fence the
    /// processes were spawned under; restart-in-place opens a new domain with
    /// the new generation rather than mutating this one.
    pub fn open_session(
        &self,
        session: &SessionName,
        generation: Generation,
    ) -> Result<SessionKillDomain, Error> {
        Ok(SessionKillDomain {
            session: session.clone(),
            generation,
            leader_pgid: None,
            tracked: Vec::new(),
            global: self.global.clone(),
            signals: self.signals.clone(),
            audit: self.audit.clone(),
        })
    }
}

/// One session's kill domain.
#[derive(Debug)]
pub struct SessionKillDomain {
    session: SessionName,
    generation: Generation,
    leader_pgid: Option<i32>,
    tracked: Vec<TrackedChild>,
    global: Arc<GlobalTracking>,
    signals: Arc<dyn ProcessSignals>,
    audit: AuditLogger,
}

impl SessionKillDomain {
    /// Record the session leader. `portable-pty` calls `setsid` in its own
    /// pre-exec hook, so the child is a session and process-group leader with
    /// `pgid == pid`; that group is Tier 1's signal path.
    pub fn set_leader(&mut self, pid: i32) -> Result<(), Error> {
        self.leader_pgid = Some(pid);
        self.track(pid)
    }

    /// Track a process for reaping and defence-in-depth signalling.
    ///
    /// Fail-closed on capacity: the caller must kill the session rather than
    /// continue with partial tracking.
    pub fn track(&mut self, pid: i32) -> Result<(), Error> {
        if self.tracked.len() >= self.global.limits.max_tracked_per_session {
            return Err(Error::CapacityExceeded {
                limit: self.global.limits.max_tracked_per_session,
            });
        }
        self.global.reserve()?;
        #[cfg(target_os = "linux")]
        let pidfd = PidFd::open(pid).ok();
        self.tracked.push(TrackedChild {
            pid,
            #[cfg(target_os = "linux")]
            pidfd,
        });
        Ok(())
    }

    /// Terminate everything in this session's domain.
    ///
    /// Bounded in time. Teardown must never be blockable by a slow client or an
    /// uncooperative child, because renew, TTL expiry, and kill all depend on it
    /// returning.
    pub async fn terminate(&mut self, grace: Duration) -> TeardownOutcome {
        let started = Instant::now();
        let survivors = self.terminate_tier1(grace).await;
        let outcome = TeardownOutcome {
            tier: KillDomainTier::Tier1BestEffortProcessGroup,
            survivors,
            elapsed_ms: started.elapsed().as_millis() as u64,
        };
        if !outcome.survivors.is_empty() {
            self.global
                .leaked
                .fetch_add(outcome.survivors.len() as u64, Ordering::Relaxed);
            // A Tier 1 survivor is the documented best-effort outcome: audited
            // as an anomaly so it is observable, not silent.
            self.audit.record(
                AuditEvent::new(AuditKind::Tier1SurvivorDetected)
                    .session(&self.session, self.generation)
                    .detail(format!(
                        "{}: {} survivor(s) after teardown; pids={:?}",
                        outcome.tier.label(),
                        outcome.survivors.len(),
                        outcome.survivors
                    )),
            );
        }
        outcome
    }

    async fn terminate_tier1(&mut self, grace: Duration) -> Vec<i32> {
        // SIGTERM over the process group first, then the same signal through each
        // tracked pidfd. The pidfd pass is defence in depth: a child that left
        // the group but is still tracked is reachable, and a pidfd cannot signal
        // a recycled pid.
        if let Some(pgid) = self.leader_pgid {
            let _ = self.signals.signal_group(pgid, Signal::Term);
        }
        self.signal_tracked(Signal::Term);

        if !self.wait_until_drained(grace).await {
            if let Some(pgid) = self.leader_pgid {
                let _ = self.signals.signal_group(pgid, Signal::Kill);
            }
            self.signal_tracked(Signal::Kill);
            self.wait_until_drained(TIER1_KILL_WAIT).await;
        }

        // Reap before reporting: a zombie is exited, not a survivor.
        self.reap_tracked();

        let mut survivors: Vec<i32> = self
            .tracked
            .iter()
            .filter(|child| self.signals.pid_alive(child.pid))
            .map(|child| child.pid)
            .collect();
        if let Some(pgid) = self.leader_pgid {
            if self.signals.group_alive(pgid) && !survivors.contains(&pgid) {
                survivors.push(pgid);
            }
        }
        survivors
    }

    fn signal_tracked(&self, signal: Signal) {
        for child in &self.tracked {
            #[cfg(target_os = "linux")]
            if let Some(pidfd) = child.pidfd.as_ref() {
                let _ = pidfd.send_signal(signal);
                continue;
            }
            let _ = self.signals.signal_pid(child.pid, signal);
        }
    }

    /// Reap every tracked pid, unconditionally.
    ///
    /// The attempt must not be gated on `!pid_alive`: a zombie is still
    /// signalable, so `kill(pid, 0)` succeeds for it and the gate would never
    /// fire. Two consequences of getting this wrong were observed for real —
    /// every teardown reported its own session leader as a survivor (making the
    /// leak metric fire unconditionally, which hides the leaks it exists to
    /// surface), and every teardown burned the full grace plus kill wait because
    /// an exited-but-unreaped child reads as alive. `waitpid` on a pid that is
    /// not our child returns `ECHILD`, which is harmless.
    fn reap_tracked(&self) {
        for child in &self.tracked {
            self.signals.reap_nonblocking(child.pid);
        }
    }

    async fn wait_until_drained(&self, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            self.reap_tracked();
            let alive = self
                .tracked
                .iter()
                .any(|child| self.signals.pid_alive(child.pid))
                || self
                    .leader_pgid
                    .is_some_and(|pgid| self.signals.group_alive(pgid));
            if !alive {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(TIER1_POLL_STEP).await;
        }
    }

    /// Release tracking slots. Called after teardown, as the last step of the
    /// session-teardown sequence.
    pub fn release(&mut self) {
        self.global.release(self.tracked.len());
        self.tracked.clear();
    }
}

impl Drop for SessionKillDomain {
    fn drop(&mut self) {
        if !self.tracked.is_empty() {
            self.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    /// A simulated process table. Behaviour, not call counting: each process has
    /// its own response to SIGTERM and may have left the process group, so the
    /// escalation ladder is asserted through the surviving state.
    #[derive(Debug)]
    struct FakeProcess {
        pid: i32,
        pgid: i32,
        /// Ignores SIGTERM; only SIGKILL ends it.
        traps_term: bool,
        /// Has not exited yet.
        running: bool,
        reaped: bool,
    }

    #[derive(Debug)]
    struct FakeSignals {
        table: Mutex<Vec<FakeProcess>>,
    }

    impl FakeSignals {
        fn new(processes: Vec<FakeProcess>) -> Arc<Self> {
            Arc::new(Self {
                table: Mutex::new(processes),
            })
        }

        /// Processes that have not exited.
        fn alive_pids(&self) -> Vec<i32> {
            self.table
                .lock()
                .iter()
                .filter(|p| p.running)
                .map(|p| p.pid)
                .collect()
        }

        fn reaped_pids(&self) -> Vec<i32> {
            self.table
                .lock()
                .iter()
                .filter(|p| p.reaped)
                .map(|p| p.pid)
                .collect()
        }

        fn deliver(&self, matches: impl Fn(&FakeProcess) -> bool, signal: Signal) {
            for process in self.table.lock().iter_mut() {
                if !process.running || !matches(process) {
                    continue;
                }
                if signal == Signal::Kill || !process.traps_term {
                    process.running = false;
                }
            }
        }
    }

    impl ProcessSignals for FakeSignals {
        fn signal_group(&self, pgid: i32, signal: Signal) -> io::Result<()> {
            self.deliver(|p| p.pgid == pgid, signal);
            Ok(())
        }

        fn signal_pid(&self, pid: i32, signal: Signal) -> io::Result<()> {
            self.deliver(|p| p.pid == pid, signal);
            Ok(())
        }

        // Liveness answers what the kernel answers: an exited-but-unreaped child
        // is a zombie, and `kill(pid, 0)` on a zombie succeeds. Modelling a
        // zombie as absent — which this fake used to do — is a diagnostic that
        // disagrees with the shipping code, and it hid two real teardown defects.
        fn group_alive(&self, pgid: i32) -> bool {
            self.table
                .lock()
                .iter()
                .any(|p| p.pgid == pgid && !p.reaped)
        }

        fn pid_alive(&self, pid: i32) -> bool {
            self.table.lock().iter().any(|p| p.pid == pid && !p.reaped)
        }

        fn reap_nonblocking(&self, pid: i32) -> bool {
            for process in self.table.lock().iter_mut() {
                if process.pid == pid && !process.running {
                    process.reaped = true;
                    return true;
                }
            }
            false
        }
    }

    fn process(pid: i32, pgid: i32, traps_term: bool) -> FakeProcess {
        FakeProcess {
            pid,
            pgid,
            traps_term,
            running: true,
            reaped: false,
        }
    }

    fn session() -> SessionName {
        SessionName::parse("kd-test").unwrap()
    }

    fn tier1_domain(signals: Arc<dyn ProcessSignals>) -> KillDomain {
        KillDomain::with_signals(TrackingLimits::default(), AuditLogger, signals)
    }

    #[test]
    fn tier_names_and_status_label_best_effort() {
        let domain = tier1_domain(FakeSignals::new(vec![]));
        let status = domain.status();
        assert!(status.teardown_best_effort);
        assert!(status.absolute_ttl_best_effort);
        assert!(status.tier.label().contains("best-effort"));
        let rendered = serde_json::to_string(&status).unwrap();
        assert!(rendered.contains("best-effort"), "{rendered}");
        assert!(domain.tier().describe().contains("BEST-EFFORT"));
    }

    #[test]
    fn tier2_required_is_refused_because_tier2_is_not_implemented() {
        // The whole reason any Tier 2 vocabulary survives: an operator who asks
        // for the hard guarantee is refused, never handed best effort under the
        // guarantee's name.
        let error = resolve_tier(KillDomainRequirement::Tier2Required)
            .expect_err("must never silently downgrade a requested guarantee");
        let message = error.to_string();
        assert!(message.contains("tier2-required"), "{message}");
        assert!(message.contains("NOT IMPLEMENTED"), "{message}");
        assert_eq!(
            resolve_tier(KillDomainRequirement::Tier1Allowed).unwrap(),
            KillDomainTier::Tier1BestEffortProcessGroup
        );
    }

    #[tokio::test]
    async fn tier1_escalates_to_sigkill_for_a_sigterm_trapping_child() {
        let signals = FakeSignals::new(vec![process(100, 100, true), process(101, 100, false)]);
        let domain = tier1_domain(signals.clone());
        let mut session_domain = domain.open_session(&session(), Generation(3)).unwrap();
        session_domain.set_leader(100).unwrap();
        session_domain.track(101).unwrap();

        let outcome = session_domain.terminate(Duration::from_millis(60)).await;

        assert!(outcome.converged(), "SIGKILL must finish the job");
        assert!(
            outcome.is_best_effort(),
            "tier 1 never claims a hard guarantee"
        );
        assert!(signals.alive_pids().is_empty());
        assert_eq!(signals.reaped_pids(), vec![100, 101], "no zombies left");
    }

    #[tokio::test]
    async fn tier1_does_not_report_its_own_reaped_leader_as_a_survivor() {
        // Regression guard for a defect found by running the binary, not the
        // suite: a zombie is signalable, so an exited-but-unreaped session leader
        // read as "alive". Every teardown therefore reported one survivor and
        // bumped the leak metric — a counter that always fires cannot surface the
        // leaks it exists for — and every teardown waited out the full grace plus
        // kill window before saying so.
        let signals = FakeSignals::new(vec![process(500, 500, false)]);
        let domain = tier1_domain(signals.clone());
        let mut session_domain = domain.open_session(&session(), Generation(1)).unwrap();
        session_domain.set_leader(500).unwrap();

        let started = std::time::Instant::now();
        let outcome = session_domain.terminate(Duration::from_secs(5)).await;

        assert!(
            outcome.converged(),
            "a child that exits on SIGTERM is not a survivor: {:?}",
            outcome.survivors
        );
        assert_eq!(domain.leaked_process_count(), 0);
        assert_eq!(
            signals.reaped_pids(),
            vec![500],
            "the leader must be reaped"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "teardown must converge as soon as the child is gone, not after the grace"
        );
    }

    #[tokio::test]
    async fn tier1_reaches_a_tracked_escapee_through_the_pidfd_pass() {
        // A double-forked child that called setsid: its own group, but the
        // runtime tracked its pid at spawn, so it is still visible.
        let signals = FakeSignals::new(vec![process(200, 200, false), process(201, 999, true)]);
        let domain = tier1_domain(signals.clone());
        let mut session_domain = domain.open_session(&session(), Generation(3)).unwrap();
        session_domain.set_leader(200).unwrap();
        session_domain.track(201).unwrap();

        // Escapee traps SIGTERM but the tracked-pid SIGKILL pass reaches it.
        let outcome = session_domain.terminate(Duration::from_millis(40)).await;
        assert!(outcome.converged());
        assert!(signals.alive_pids().is_empty());
        assert_eq!(domain.leaked_process_count(), 0);
    }

    #[tokio::test]
    async fn tier1_leaks_are_counted_and_audited_when_a_survivor_remains() {
        // A survivor that ignores every signal we can send stands in for the
        // process the runtime cannot reach.
        #[derive(Debug)]
        struct Immortal;
        impl ProcessSignals for Immortal {
            fn signal_group(&self, _pgid: i32, _signal: Signal) -> io::Result<()> {
                Ok(())
            }
            fn signal_pid(&self, _pid: i32, _signal: Signal) -> io::Result<()> {
                Ok(())
            }
            fn group_alive(&self, _pgid: i32) -> bool {
                true
            }
            fn pid_alive(&self, _pid: i32) -> bool {
                true
            }
            fn reap_nonblocking(&self, _pid: i32) -> bool {
                false
            }
        }

        let domain = tier1_domain(Arc::new(Immortal));
        let mut session_domain = domain.open_session(&session(), Generation(3)).unwrap();
        session_domain.set_leader(300).unwrap();

        let outcome = session_domain.terminate(Duration::from_millis(30)).await;
        assert!(!outcome.converged());
        assert_eq!(outcome.survivors, vec![300]);
        assert_eq!(
            domain.leaked_process_count(),
            1,
            "the leak metric is what makes best-effort observable"
        );
    }

    #[tokio::test]
    async fn tier1_cannot_see_an_untracked_pgid_escapee() {
        // The ADR's honest limitation, asserted rather than assumed: a process
        // that left the group and was never tracked is invisible, so teardown
        // reports no survivors while the process is still alive. This is why the
        // tier is labelled best-effort and why the pod/task is the reclamation
        // path.
        let signals = FakeSignals::new(vec![process(400, 400, false), process(401, 777, false)]);
        let domain = tier1_domain(signals.clone());
        let mut session_domain = domain.open_session(&session(), Generation(3)).unwrap();
        session_domain.set_leader(400).unwrap();

        let outcome = session_domain.terminate(Duration::from_millis(30)).await;
        assert!(outcome.converged(), "nothing visible survived");
        assert_eq!(
            signals.alive_pids(),
            vec![401],
            "the escapee outlives its session"
        );
        assert!(outcome.is_best_effort());
    }

    #[test]
    fn tracking_capacity_is_fail_closed_per_session_and_globally() {
        let per_session = KillDomain::with_signals(
            TrackingLimits {
                max_tracked_per_session: 2,
                max_tracked_global: 100,
                reserved_fd_headroom: 0,
            },
            AuditLogger,
            FakeSignals::new(vec![]),
        );
        let mut domain = per_session.open_session(&session(), Generation(3)).unwrap();
        domain.track(1).unwrap();
        domain.track(2).unwrap();
        assert!(
            matches!(domain.track(3), Err(Error::CapacityExceeded { limit: 2 })),
            "per-session cap must reject rather than track partially"
        );

        let global = KillDomain::with_signals(
            TrackingLimits {
                max_tracked_per_session: 10,
                max_tracked_global: 1,
                reserved_fd_headroom: 0,
            },
            AuditLogger,
            FakeSignals::new(vec![]),
        );
        let mut a = global
            .open_session(&SessionName::parse("a").unwrap(), Generation(1))
            .unwrap();
        let mut b = global
            .open_session(&SessionName::parse("b").unwrap(), Generation(1))
            .unwrap();
        a.track(10).unwrap();
        assert!(matches!(b.track(11), Err(Error::CapacityExceeded { .. })));

        // Releasing a session returns its budget.
        a.release();
        b.track(11).unwrap();
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_never_claims_a_subreaper() {
        assert_eq!(establish_subreaper(), SubreaperStatus::Unsupported);
    }

    // ---- integration: real processes ---------------------------------------
    // Ignored by default. This is the only test here that leaves the process.

    #[tokio::test]
    #[ignore = "spawns real processes"]
    async fn tier1_teardown_against_a_real_process_group() {
        let domain = KillDomain::new(TrackingLimits::default(), AuditLogger);
        let mut session_domain = domain
            .open_session(&SessionName::parse("real-tier1").unwrap(), Generation(1))
            .unwrap();
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .spawn()
            .expect("spawn a real child");
        let pid = child.id() as i32;
        session_domain.set_leader(pid).unwrap();

        let outcome = session_domain.terminate(Duration::from_secs(2)).await;
        assert!(outcome.converged(), "survivors: {:?}", outcome.survivors);
        assert!(outcome.is_best_effort());
        let _ = child.wait();
    }
}

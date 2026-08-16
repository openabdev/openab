//! The kill domain: two tiers, selected by startup detection.
//!
//! The ADR's kill-domain section rests on measured kernel behaviour, and the two
//! problems it names are solved separately here:
//!
//! * **Attribution** — which session does a live process belong to? Only the
//!   kernel can answer this. Tier 2 asks it (`cgroup.procs`). Tier 1 cannot, and
//!   says so.
//! * **Reaping** — collecting exited children. `PR_SET_CHILD_SUBREAPER` plus a
//!   `pidfd` per tracked child covers this in both tiers.
//!
//! Ancestry is never used for attribution. The spike measured two concurrent
//! sessions each leaking a double-fork orphan whose intermediate exited before
//! any scan: **both** orphans reparented to the runtime and neither was
//! attributable to its session, while reaping of both intermediates succeeded.
//! Reaping succeeds exactly where attribution fails, so lineage is only ever
//! used to *reap*, never to decide who owns a process.
//!
//! | Tier | Mechanism | Teardown convergence | Absolute TTL |
//! |---|---|---|---|
//! | [`KillDomainTier::Tier1BestEffortProcessGroup`] | `setsid`/pgid + `SIGTERM`→`SIGKILL`, subreaper, pidfd | **best effort** | **best effort** |
//! | [`KillDomainTier::Tier2GuaranteedCgroup`] | one cgroup per session, `cgroup.kill` | hard guarantee | hard guarantee |
//!
//! Tier 1 is best-effort because a child that calls `setsid` or double-forks
//! leaves the process group, and nothing short of a kernel-authoritative
//! membership primitive can find it again. Such a process may outlive its
//! session until the pod or task is replaced — that is the architecture's normal
//! reclamation path, not a bug. Survivors we *can* see are audited as anomalies
//! and counted on [`KillDomain::leaked_process_count`] so the condition is
//! observable rather than silent, and every type name, doc comment, and status
//! field on this path carries the best-effort label.

use crate::audit::{AuditEvent, AuditKind, AuditLogger};
use crate::config::KillDomainRequirement;
use crate::{Error, Generation, SessionName};
use serde::Serialize;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::io::{AsRawFd, RawFd};

/// Polling step while waiting for a process group to drain. Small enough that
/// teardown is not perceptibly slower than the kernel, large enough not to spin.
const TIER1_POLL_STEP: Duration = Duration::from_millis(25);
/// Polling step for cgroup emptiness. `cgroup.kill` was measured to converge in
/// ~10 ms, so this resolves in one or two iterations.
const TIER2_POLL_STEP: Duration = Duration::from_millis(5);
/// Bound on the post-`SIGKILL` drain wait. `SIGKILL` is not catchable, so this
/// only covers scheduling latency.
const TIER1_KILL_WAIT: Duration = Duration::from_millis(500);
/// Bound on the Tier 2 wait-for-empty. Exceeding it is a kernel-level anomaly,
/// and a bounded wait is what keeps teardown non-blockable by a client.
const TIER2_CONVERGENCE_BUDGET: Duration = Duration::from_secs(2);

/// The active containment tier. The variant names carry the guarantee because
/// this value reaches operators through logs and status output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KillDomainTier {
    /// Default. No prerequisites, and **best-effort** teardown/TTL.
    Tier1BestEffortProcessGroup,
    /// Optional hardening. Requires delegated cgroup v2 reaching the container's
    /// own cgroup; teardown convergence becomes a hard guarantee.
    Tier2GuaranteedCgroup,
}

impl KillDomainTier {
    /// True when convergence is not guaranteed. Callers must surface this.
    pub fn is_best_effort(self) -> bool {
        matches!(self, Self::Tier1BestEffortProcessGroup)
    }

    pub fn guarantee(self) -> TeardownGuarantee {
        match self {
            Self::Tier1BestEffortProcessGroup => TeardownGuarantee::BestEffort,
            Self::Tier2GuaranteedCgroup => TeardownGuarantee::Hard,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Tier1BestEffortProcessGroup => "tier1-best-effort-process-group",
            Self::Tier2GuaranteedCgroup => "tier2-guaranteed-cgroup",
        }
    }
}

impl fmt::Display for KillDomainTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Whether teardown convergence is guaranteed. Kept as its own type so a
/// best-effort outcome cannot be mistaken for a successful containment claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TeardownGuarantee {
    BestEffort,
    Hard,
}

/// Why Tier 2 is not active. The distinguishing reason is required by the ADR:
/// each of these points at a different operator remedy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "reason", content = "detail")]
pub enum Tier2Unavailable {
    /// `mkdir` returned `EROFS`: no writable cgroup mount. Denied at the mount,
    /// so ownership is irrelevant — the default container contract lands here.
    NoWritableCgroupMount,
    /// `EACCES`/`EPERM`: a cgroup mount exists but the subtree is not delegated
    /// to this container's own cgroup.
    SubtreeNotDelegated,
    /// `ENOENT`: the path is outside this container's cgroup namespace (a
    /// pod-level sibling fails here under `nsdelegate`).
    OutsideCgroupNamespace,
    /// A required syscall was filtered (seccomp commonly blocks `prctl` and
    /// `pidfd_open`). The probe is the intended detection for those setups.
    SyscallBlocked(String),
    /// Non-Linux target: cgroups do not exist. Never reported as containment.
    NotLinux,
    /// The active PTY spawner cannot write the child's pid to `cgroup.procs`
    /// before `execv`. Distinct from the kernel reasons above: the kernel is
    /// willing, the spawn primitive is not, and a post-`exec` migration is
    /// refused because adversary code would then have run outside its cgroup.
    SpawnerCannotJoinCgroupPreExec,
    /// The end-to-end probe (create → populate → `cgroup.kill` → remove) failed
    /// for a reason not covered above.
    ProbeFailed(String),
}

impl fmt::Display for Tier2Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoWritableCgroupMount => f.write_str("EROFS: no writable cgroup2 mount"),
            Self::SubtreeNotDelegated => {
                f.write_str("EACCES: cgroup subtree is not delegated to this container")
            }
            Self::OutsideCgroupNamespace => {
                f.write_str("ENOENT: path is outside this container's cgroup namespace")
            }
            Self::SyscallBlocked(what) => write!(f, "syscall blocked: {what}"),
            Self::NotLinux => f.write_str("not Linux: cgroup containment does not exist here"),
            Self::SpawnerCannotJoinCgroupPreExec => f.write_str(
                "spawner cannot write cgroup.procs before execv; refusing post-exec migration",
            ),
            Self::ProbeFailed(what) => write!(f, "probe failed: {what}"),
        }
    }
}

/// What the PTY spawner is able to do at fork time. Tier 2's guarantee depends
/// on the pid landing in the cgroup *before* `execv`, so the spawner's
/// capability is part of tier selection rather than an afterthought.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnCapability {
    /// Can run a pre-`execv` hook and write the child's own pid to a pre-opened
    /// `cgroup.procs` descriptor.
    PreExecCgroupJoin,
    /// Can only place the child in its own process group.
    ProcessGroupOnly,
}

/// Outcome of tier selection.
#[derive(Clone, Debug)]
pub struct KillDomainSelection {
    pub tier: KillDomainTier,
    /// Present whenever Tier 2 is not active; carries the distinguishing reason.
    pub tier2_unavailable: Option<Tier2Unavailable>,
    /// The delegated cgroup directory session cgroups are created under. Always
    /// a descendant of the runtime's own cgroup.
    pub cgroup_base: Option<PathBuf>,
}

impl KillDomainSelection {
    pub fn tier1(reason: Tier2Unavailable) -> Self {
        Self {
            tier: KillDomainTier::Tier1BestEffortProcessGroup,
            tier2_unavailable: Some(reason),
            cgroup_base: None,
        }
    }

    /// One log line an operator can act on: which tier is active and, when it is
    /// the best-effort one, exactly why the hard guarantee is unavailable.
    pub fn describe(&self) -> String {
        match &self.tier2_unavailable {
            None => format!("kill domain: {} (convergence guaranteed)", self.tier),
            Some(reason) => format!(
                "kill domain: {} (teardown and absolute TTL are BEST-EFFORT); tier 2 unavailable: {reason}",
                self.tier
            ),
        }
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
        unsafe { libc::kill(-pgid, 0) == 0 }
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

/// One session's cgroup, addressed through a directory handle the runtime itself
/// created.
///
/// Membership is read from `<session>/cgroup.procs`. We deliberately never parse
/// `/proc/<pid>/cgroup`: it reports a cgroup-namespace-relative path, not the
/// mount-relative path the runtime used, so string matching against it is
/// fragile in exactly the environments Tier 2 targets.
#[derive(Debug)]
pub struct SessionCgroup {
    dir: PathBuf,
    #[cfg(unix)]
    dir_handle: std::fs::File,
    /// Pre-opened `cgroup.procs` writer for the pre-`execv` pid write. Opened
    /// `O_CLOEXEC` on purpose: the pre-exec hook writes and closes it, and it
    /// must never survive into the child, where it would be a handle for moving
    /// arbitrary pids into this cgroup.
    #[cfg(unix)]
    procs_writer: Option<std::fs::File>,
}

impl SessionCgroup {
    /// Create `<base>/oab-pty-<session>`.
    ///
    /// `base` must be the runtime's own cgroup (or a descendant): only a
    /// descendant of it satisfies cgroup v2 delegation containment. The runtime
    /// itself stays in `base` while children live in this leaf, which is legal
    /// because Tier 2 needs no *controller* delegation — `cgroup.procs`,
    /// `cgroup.events` and `cgroup.kill` are core files.
    pub fn create(base: &Path, session: &SessionName) -> io::Result<Self> {
        let dir = base.join(format!("oab-pty-{session}"));
        match std::fs::create_dir(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        Ok(Self {
            #[cfg(unix)]
            dir_handle: std::fs::File::open(&dir)?,
            #[cfg(unix)]
            procs_writer: None,
            dir,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Pre-open `cgroup.procs` for the pre-`execv` write and return its raw fd.
    #[cfg(unix)]
    pub fn open_procs_for_write(&mut self) -> io::Result<RawFd> {
        if self.procs_writer.is_none() {
            self.procs_writer = Some(self.open_at("cgroup.procs", true)?);
        }
        Ok(self.procs_writer.as_ref().expect("just opened").as_raw_fd())
    }

    /// Kernel-authoritative membership.
    pub fn members(&self) -> io::Result<Vec<i32>> {
        let content = self.read_at("cgroup.procs")?;
        Ok(parse_cgroup_procs(&content))
    }

    /// `cgroup.kill`: the kernel kills every member and closes the fork race in
    /// kernel space, which is why no userspace kill-and-rescan loop is needed.
    pub fn kill(&self) -> io::Result<()> {
        self.write_at("cgroup.kill", b"1")
    }

    /// Remove the (empty) cgroup and drop the pre-opened descriptor.
    pub fn remove(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.procs_writer = None;
        }
        match std::fs::remove_dir(&self.dir) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    #[cfg(unix)]
    fn open_at(&self, name: &str, write: bool) -> io::Result<std::fs::File> {
        use std::ffi::CString;
        use std::os::unix::io::FromRawFd;
        let name = CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let flags = if write {
            libc::O_WRONLY | libc::O_CLOEXEC
        } else {
            libc::O_RDONLY | libc::O_CLOEXEC
        };
        let fd = unsafe { libc::openat(self.dir_handle.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }

    fn read_at(&self, name: &str) -> io::Result<String> {
        #[cfg(unix)]
        {
            use std::io::Read;
            let mut file = self.open_at(name, false)?;
            let mut content = String::new();
            file.read_to_string(&mut content)?;
            Ok(content)
        }
        #[cfg(not(unix))]
        {
            std::fs::read_to_string(self.dir.join(name))
        }
    }

    fn write_at(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::io::Write;
            let mut file = self.open_at(name, true)?;
            file.write_all(bytes)
        }
        #[cfg(not(unix))]
        {
            std::fs::write(self.dir.join(name), bytes)
        }
    }
}

fn parse_cgroup_procs(content: &str) -> Vec<i32> {
    content
        .lines()
        .filter_map(|line| line.trim().parse::<i32>().ok())
        .filter(|pid| *pid > 0)
        .collect()
}

/// Write the calling process's own pid to a pre-opened `cgroup.procs`
/// descriptor. Intended to run in a pre-`execv` hook.
///
/// Async-signal-safety is a hard requirement here: between `fork` and `execv`
/// only the calling thread exists, so any allocation could deadlock on an
/// allocator lock held by another thread at fork time. This function therefore
/// formats the pid into a stack buffer and performs only `getpid`/`write`/`close`.
///
/// # Safety
/// `procs_fd` must be a valid descriptor open for writing, and the caller must
/// be in the pre-`execv` window of its own child.
#[cfg(unix)]
pub unsafe fn preexec_join_cgroup(procs_fd: RawFd) -> io::Result<()> {
    let pid = libc::getpid();
    let mut buf = [0u8; 24];
    let len = format_pid_decimal(pid, &mut buf);
    let mut written = 0usize;
    while written < len {
        let rc = libc::write(
            procs_fd,
            buf.as_ptr().add(written) as *const libc::c_void,
            len - written,
        );
        if rc < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
        if rc == 0 {
            return Err(io::Error::from(io::ErrorKind::WriteZero));
        }
        written += rc as usize;
    }
    // Close rather than leak the descriptor into the child: it is authority to
    // move pids into this cgroup.
    libc::close(procs_fd);
    Ok(())
}

/// Allocation-free decimal formatting for the pre-exec hook. Returns the number
/// of bytes written at the start of `buf`.
fn format_pid_decimal(pid: i32, buf: &mut [u8; 24]) -> usize {
    if pid == 0 {
        buf[0] = b'0';
        return 1;
    }
    let negative = pid < 0;
    let mut value = pid.unsigned_abs() as u64;
    let mut digits = [0u8; 24];
    let mut n = 0;
    while value > 0 {
        digits[n] = b'0' + (value % 10) as u8;
        value /= 10;
        n += 1;
    }
    let mut len = 0;
    if negative {
        buf[0] = b'-';
        len = 1;
    }
    for i in 0..n {
        buf[len + i] = digits[n - 1 - i];
    }
    len + n
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

    /// Self-exit detection without a blocking wait: a `pidfd` becomes readable
    /// when its process exits.
    fn exited(&self) -> bool {
        let mut poll = libc::pollfd {
            fd: self.0,
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut poll, 1, 0) > 0 && poll.revents & libc::POLLIN != 0 }
    }
}

#[cfg(target_os = "linux")]
impl Drop for PidFd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

/// Result of a teardown. `guarantee` is what distinguishes "we saw no
/// survivors" from "there can be no survivors".
#[derive(Clone, Debug, Serialize)]
pub struct TeardownOutcome {
    pub tier: KillDomainTier,
    pub guarantee: TeardownGuarantee,
    /// Processes still alive after escalation. In Tier 1 this is only the
    /// *visible* remainder: pgid escapees the runtime never tracked are, by
    /// construction, invisible here.
    pub survivors: Vec<i32>,
    pub elapsed_ms: u64,
}

impl TeardownOutcome {
    /// True when nothing visible survived. In Tier 1 this is still a best-effort
    /// statement — see [`TeardownOutcome::guarantee`].
    pub fn converged(&self) -> bool {
        self.survivors.is_empty()
    }

    pub fn is_best_effort(&self) -> bool {
        self.guarantee == TeardownGuarantee::BestEffort
    }
}

/// Manager-level kill domain: the selected tier plus global tracking budgets.
#[derive(Debug)]
pub struct KillDomain {
    selection: KillDomainSelection,
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
    pub teardown_guarantee: TeardownGuarantee,
    /// Duplicated as an explicit boolean so a status consumer cannot miss it.
    pub teardown_best_effort: bool,
    pub absolute_ttl_best_effort: bool,
    pub tier2_unavailable: Option<Tier2Unavailable>,
    pub subreaper: SubreaperStatus,
    pub tracked_processes: usize,
    pub tracked_process_ceiling: usize,
    /// Tier 1 leak metric: visible survivors observed across all teardowns.
    pub leaked_processes: u64,
}

impl KillDomain {
    pub fn new(selection: KillDomainSelection, limits: TrackingLimits, audit: AuditLogger) -> Self {
        Self::with_signals(selection, limits, audit, Arc::new(SystemSignals))
    }

    pub fn with_signals(
        selection: KillDomainSelection,
        limits: TrackingLimits,
        audit: AuditLogger,
        signals: Arc<dyn ProcessSignals>,
    ) -> Self {
        Self {
            selection,
            subreaper: establish_subreaper(),
            global: Arc::new(GlobalTracking::new(limits)),
            signals,
            audit,
        }
    }

    pub fn tier(&self) -> KillDomainTier {
        self.selection.tier
    }

    pub fn selection(&self) -> &KillDomainSelection {
        &self.selection
    }

    pub fn leaked_process_count(&self) -> u64 {
        self.global.leaked.load(Ordering::Relaxed)
    }

    pub fn status(&self) -> KillDomainStatus {
        let best_effort = self.selection.tier.is_best_effort();
        KillDomainStatus {
            tier: self.selection.tier,
            teardown_guarantee: self.selection.tier.guarantee(),
            teardown_best_effort: best_effort,
            // The absolute TTL depends on teardown actually reclaiming the
            // session, so it inherits the same qualifier.
            absolute_ttl_best_effort: best_effort,
            tier2_unavailable: self.selection.tier2_unavailable.clone(),
            subreaper: self.subreaper.clone(),
            tracked_processes: self.global.tracked.load(Ordering::Relaxed),
            tracked_process_ceiling: self.global.ceiling(),
            leaked_processes: self.leaked_process_count(),
        }
    }

    /// Prepare a session's kill domain. In Tier 2 this creates the session
    /// cgroup and pre-opens `cgroup.procs` so the spawner can join before
    /// `execv`.
    ///
    /// `generation` is carried only so anomaly audits name the fence the
    /// processes were spawned under; restart-in-place opens a new domain with
    /// the new generation rather than mutating this one.
    pub fn open_session(
        &self,
        session: &SessionName,
        generation: Generation,
    ) -> Result<SessionKillDomain, Error> {
        let cgroup = match (self.selection.tier, self.selection.cgroup_base.as_ref()) {
            (KillDomainTier::Tier2GuaranteedCgroup, Some(base)) => {
                Some(SessionCgroup::create(base, session)?)
            }
            (KillDomainTier::Tier2GuaranteedCgroup, None) => {
                return Err(Error::Other(
                    "tier 2 selected without a delegated cgroup base".into(),
                ))
            }
            _ => None,
        };
        Ok(SessionKillDomain {
            session: session.clone(),
            generation,
            tier: self.selection.tier,
            cgroup,
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
    tier: KillDomainTier,
    cgroup: Option<SessionCgroup>,
    leader_pgid: Option<i32>,
    tracked: Vec<TrackedChild>,
    global: Arc<GlobalTracking>,
    signals: Arc<dyn ProcessSignals>,
    audit: AuditLogger,
}

impl SessionKillDomain {
    pub fn tier(&self) -> KillDomainTier {
        self.tier
    }

    /// Raw `cgroup.procs` descriptor for a pre-`execv` capable spawner.
    /// `None` in Tier 1, where there is no cgroup to join.
    #[cfg(unix)]
    pub fn procs_fd(&mut self) -> Result<Option<RawFd>, Error> {
        match self.cgroup.as_mut() {
            Some(cgroup) => Ok(Some(cgroup.open_procs_for_write()?)),
            None => Ok(None),
        }
    }

    /// Record the session leader. `portable-pty` calls `setsid` in its own
    /// pre-exec hook, so the child is a session and process-group leader with
    /// `pgid == pid`; that group is Tier 1's signal path.
    pub fn set_leader(&mut self, pid: i32) -> Result<(), Error> {
        self.leader_pgid = Some(pid);
        self.track(pid)
    }

    pub fn leader_pgid(&self) -> Option<i32> {
        self.leader_pgid
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

    pub fn tracked_pids(&self) -> Vec<i32> {
        self.tracked.iter().map(|child| child.pid).collect()
    }

    /// Live membership.
    ///
    /// Tier 2 asks the kernel. Tier 1 can only report tracked pids that are
    /// still alive plus, if the leader's group still exists, the leader — an
    /// escapee that left the group is not discoverable at all.
    pub fn members(&self) -> Result<Vec<i32>, Error> {
        match self.cgroup.as_ref() {
            Some(cgroup) => Ok(cgroup.members()?),
            None => Ok(self
                .tracked
                .iter()
                .filter(|child| self.signals.pid_alive(child.pid))
                .map(|child| child.pid)
                .collect()),
        }
    }

    /// Whether the tracked leader has exited, without blocking. On Linux this
    /// uses the leader's `pidfd`, which cannot be confused by pid reuse.
    pub fn leader_exited(&self) -> bool {
        let Some(pgid) = self.leader_pgid else {
            return false;
        };
        #[cfg(target_os = "linux")]
        if let Some(child) = self.tracked.iter().find(|child| child.pid == pgid) {
            if let Some(pidfd) = child.pidfd.as_ref() {
                return pidfd.exited();
            }
        }
        !self.signals.pid_alive(pgid)
    }

    /// Terminate everything in this session's domain.
    ///
    /// Both paths are bounded in time. Teardown must never be blockable by a
    /// slow client or an uncooperative child, because renew, TTL expiry, and
    /// kill all depend on it returning.
    pub async fn terminate(&mut self, grace: Duration) -> TeardownOutcome {
        let started = Instant::now();
        let survivors = match self.tier {
            KillDomainTier::Tier2GuaranteedCgroup => {
                self.terminate_tier2(grace.min(TIER2_CONVERGENCE_BUDGET)).await
            }
            KillDomainTier::Tier1BestEffortProcessGroup => self.terminate_tier1(grace).await,
        };
        let outcome = TeardownOutcome {
            tier: self.tier,
            guarantee: self.tier.guarantee(),
            survivors,
            elapsed_ms: started.elapsed().as_millis() as u64,
        };
        if !outcome.survivors.is_empty() {
            self.global
                .leaked
                .fetch_add(outcome.survivors.len() as u64, Ordering::Relaxed);
            let kind = match self.tier {
                // A Tier 1 survivor is the documented best-effort outcome:
                // audited as an anomaly so it is observable, not silent.
                KillDomainTier::Tier1BestEffortProcessGroup => AuditKind::Tier1SurvivorDetected,
                // Tier 2 not converging is a kernel-level anomaly. The frozen
                // audit vocabulary has no dedicated kind, so it rides on the
                // kill event with an unambiguous detail label.
                KillDomainTier::Tier2GuaranteedCgroup => AuditKind::SessionKill,
            };
            self.audit.record(
                AuditEvent::new(kind)
                    .session(&self.session, self.generation)
                    .detail(format!(
                        "{}: {} survivor(s) after teardown; pids={:?}",
                        self.tier.label(),
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
        for child in &self.tracked {
            if !self.signals.pid_alive(child.pid) {
                self.signals.reap_nonblocking(child.pid);
            }
        }

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

    async fn wait_until_drained(&self, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        loop {
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

    async fn terminate_tier2(&mut self, budget: Duration) -> Vec<i32> {
        let Some(cgroup) = self.cgroup.as_ref() else {
            return Vec::new();
        };
        if let Err(error) = cgroup.kill() {
            self.audit.record(
                AuditEvent::new(AuditKind::SessionKill)
                    .session(&self.session, self.generation)
                    .detail(format!("cgroup.kill write failed: {error}")),
            );
        }
        // No userspace kill-and-rescan loop: cgroup.kill closes the fork race in
        // the kernel, so waiting for the cgroup to empty is sufficient. The wait
        // is still bounded, because an unbounded wait here would let teardown
        // hang renew and TTL expiry.
        let deadline = Instant::now() + budget;
        loop {
            let members = cgroup.members().unwrap_or_default();
            if members.is_empty() {
                // Reap tracked children so no zombie holds the pid.
                for child in &self.tracked {
                    self.signals.reap_nonblocking(child.pid);
                }
                return Vec::new();
            }
            if Instant::now() >= deadline {
                return members;
            }
            tokio::time::sleep(TIER2_POLL_STEP).await;
        }
    }

    /// Release tracking slots and remove the session cgroup. Called after
    /// teardown, as the last step of the session-teardown sequence.
    pub fn release(&mut self) {
        self.global.release(self.tracked.len());
        self.tracked.clear();
        if let Some(cgroup) = self.cgroup.as_mut() {
            if let Err(error) = cgroup.remove() {
                tracing::warn!(
                    session = %self.session,
                    error = %error,
                    "failed to remove session cgroup"
                );
            }
        }
        self.cgroup = None;
    }
}

impl Drop for SessionKillDomain {
    fn drop(&mut self) {
        if !self.tracked.is_empty() || self.cgroup.is_some() {
            self.release();
        }
    }
}

/// Select the tier from the operator requirement, the probe result, and the
/// spawner's capability.
///
/// Pure so every combination — including the two fail-closed ones — is testable
/// without a cgroup mount.
pub fn resolve_tier(
    requirement: KillDomainRequirement,
    probe: Result<PathBuf, Tier2Unavailable>,
    capability: SpawnCapability,
) -> Result<KillDomainSelection, Error> {
    match (requirement, probe, capability) {
        (_, Ok(base), SpawnCapability::PreExecCgroupJoin) => Ok(KillDomainSelection {
            tier: KillDomainTier::Tier2GuaranteedCgroup,
            tier2_unavailable: None,
            cgroup_base: Some(base),
        }),
        // Delegation exists but the spawner cannot join pre-`execv`. Requiring
        // Tier 2 must not silently become Tier 1.
        (KillDomainRequirement::Tier2Required, Ok(_), SpawnCapability::ProcessGroupOnly) => {
            Err(Error::Config(format!(
                "kill_domain_tier = tier2-required, but {}",
                Tier2Unavailable::SpawnerCannotJoinCgroupPreExec
            )))
        }
        (KillDomainRequirement::Tier1Allowed, Ok(_), SpawnCapability::ProcessGroupOnly) => Ok(
            KillDomainSelection::tier1(Tier2Unavailable::SpawnerCannotJoinCgroupPreExec),
        ),
        (KillDomainRequirement::Tier2Required, Err(reason), _) => Err(Error::Config(format!(
            "kill_domain_tier = tier2-required, but tier 2 is unavailable: {reason}"
        ))),
        (KillDomainRequirement::Tier1Allowed, Err(reason), _) => {
            Ok(KillDomainSelection::tier1(reason))
        }
    }
}

/// Startup detection: probe Tier 2 end to end, then select the tier.
///
/// The caller logs [`KillDomainSelection::describe`], which names the active
/// tier and, when the guarantee is unavailable, the distinguishing reason.
pub fn detect(
    requirement: KillDomainRequirement,
    capability: SpawnCapability,
) -> Result<KillDomainSelection, Error> {
    resolve_tier(requirement, probe_tier2(), capability)
}

/// End-to-end Tier 2 probe: create → populate → `cgroup.kill` → remove.
///
/// A create-only probe would be misleading; the spike found the interesting
/// failures downstream of `mkdir`.
pub fn probe_tier2() -> Result<PathBuf, Tier2Unavailable> {
    #[cfg(target_os = "linux")]
    {
        linux_probe::run()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(Tier2Unavailable::NotLinux)
    }
}

#[cfg(target_os = "linux")]
mod linux_probe {
    use super::*;

    const MOUNT: &str = "/sys/fs/cgroup";

    pub(super) fn run() -> Result<PathBuf, Tier2Unavailable> {
        let base = own_cgroup_dir()?;
        let dir = base.join(format!("oab-pty-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir(&dir);
        if let Err(error) = std::fs::create_dir(&dir) {
            return Err(classify_mkdir(&error));
        }
        let outcome = populate_kill_and_check(&dir);
        let _ = std::fs::remove_dir(&dir);
        outcome.map(|()| base)
    }

    /// Locate the runtime's own cgroup directory.
    ///
    /// `/proc/self/cgroup` is read **only** to locate ourselves, never to
    /// attribute a process to a session: the path it returns is
    /// cgroup-namespace-relative, so it is unreliable for matching but adequate
    /// as a hint that we then verify against the mount. When the hint does not
    /// resolve (the usual case inside a cgroup namespace, where our own cgroup
    /// *is* the mount root) we fall back to the mount root itself.
    fn own_cgroup_dir() -> Result<PathBuf, Tier2Unavailable> {
        let mount = Path::new(MOUNT);
        if !mount.exists() {
            return Err(Tier2Unavailable::OutsideCgroupNamespace);
        }
        if let Ok(content) = std::fs::read_to_string("/proc/self/cgroup") {
            for line in content.lines() {
                if let Some(relative) = line.strip_prefix("0::") {
                    let candidate = mount.join(relative.trim_start_matches('/'));
                    if candidate.join("cgroup.procs").exists() {
                        return Ok(candidate);
                    }
                }
            }
        }
        if mount.join("cgroup.procs").exists() {
            return Ok(mount.to_path_buf());
        }
        Err(Tier2Unavailable::OutsideCgroupNamespace)
    }

    fn classify_mkdir(error: &io::Error) -> Tier2Unavailable {
        match error.raw_os_error() {
            Some(libc::EROFS) => Tier2Unavailable::NoWritableCgroupMount,
            Some(libc::EACCES) | Some(libc::EPERM) => Tier2Unavailable::SubtreeNotDelegated,
            Some(libc::ENOENT) => Tier2Unavailable::OutsideCgroupNamespace,
            _ => Tier2Unavailable::ProbeFailed(error.to_string()),
        }
    }

    /// Fork a child that joins the probe cgroup and parks, then kill the cgroup
    /// and confirm it empties. This runs once, at startup, before serving.
    fn populate_kill_and_check(dir: &Path) -> Result<(), Tier2Unavailable> {
        for required in ["cgroup.procs", "cgroup.kill", "cgroup.events"] {
            if !dir.join(required).exists() {
                return Err(Tier2Unavailable::ProbeFailed(format!(
                    "{required} missing: not a cgroup v2 directory"
                )));
            }
        }
        let procs = std::fs::OpenOptions::new()
            .write(true)
            .open(dir.join("cgroup.procs"))
            .map_err(|error| match error.raw_os_error() {
                Some(libc::EACCES) | Some(libc::EPERM) => Tier2Unavailable::SubtreeNotDelegated,
                _ => Tier2Unavailable::ProbeFailed(error.to_string()),
            })?;
        let procs_fd = procs.as_raw_fd();

        let pid = unsafe { libc::fork() };
        if pid < 0 {
            let error = io::Error::last_os_error();
            return Err(Tier2Unavailable::SyscallBlocked(format!("fork: {error}")));
        }
        if pid == 0 {
            // Child: async-signal-safe only, then park until killed.
            let joined = unsafe { preexec_join_cgroup(procs_fd) };
            if joined.is_err() {
                unsafe { libc::_exit(1) };
            }
            loop {
                unsafe { libc::pause() };
            }
        }

        let cgroup = ProbeCgroup { dir: dir.to_path_buf() };
        let populated = wait_for(Duration::from_millis(500), || {
            cgroup.members().contains(&pid)
        });
        if !populated {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            reap(pid);
            return Err(Tier2Unavailable::ProbeFailed(
                "child never appeared in cgroup.procs".into(),
            ));
        }
        if let Err(error) = std::fs::write(dir.join("cgroup.kill"), b"1") {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            reap(pid);
            return Err(Tier2Unavailable::ProbeFailed(format!(
                "cgroup.kill write failed: {error}"
            )));
        }
        let emptied = wait_for(Duration::from_millis(500), || {
            cgroup.members().is_empty()
        });
        reap(pid);
        if emptied {
            Ok(())
        } else {
            Err(Tier2Unavailable::ProbeFailed(
                "cgroup did not empty after cgroup.kill".into(),
            ))
        }
    }

    struct ProbeCgroup {
        dir: PathBuf,
    }

    impl ProbeCgroup {
        fn members(&self) -> Vec<i32> {
            std::fs::read_to_string(self.dir.join("cgroup.procs"))
                .map(|content| parse_cgroup_procs(&content))
                .unwrap_or_default()
        }
    }

    fn wait_for(budget: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            if condition() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn reap(pid: i32) {
        let mut status: libc::c_int = 0;
        unsafe { libc::waitpid(pid, &mut status, 0) };
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
        alive: bool,
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

        fn alive_pids(&self) -> Vec<i32> {
            self.table
                .lock()
                .iter()
                .filter(|p| p.alive)
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
                if !process.alive || !matches(process) {
                    continue;
                }
                if signal == Signal::Kill || !process.traps_term {
                    process.alive = false;
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

        fn group_alive(&self, pgid: i32) -> bool {
            self.table.lock().iter().any(|p| p.alive && p.pgid == pgid)
        }

        fn pid_alive(&self, pid: i32) -> bool {
            self.table.lock().iter().any(|p| p.alive && p.pid == pid)
        }

        fn reap_nonblocking(&self, pid: i32) -> bool {
            for process in self.table.lock().iter_mut() {
                if process.pid == pid && !process.alive {
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
            alive: true,
            reaped: false,
        }
    }

    fn session() -> SessionName {
        SessionName::parse("kd-test").unwrap()
    }

    fn tier1_domain(signals: Arc<dyn ProcessSignals>) -> KillDomain {
        KillDomain::with_signals(
            KillDomainSelection::tier1(Tier2Unavailable::NotLinux),
            TrackingLimits::default(),
            AuditLogger,
            signals,
        )
    }

    #[test]
    fn tier_names_and_status_label_best_effort() {
        let domain = tier1_domain(FakeSignals::new(vec![]));
        let status = domain.status();
        assert!(status.teardown_best_effort);
        assert!(status.absolute_ttl_best_effort);
        assert_eq!(status.teardown_guarantee, TeardownGuarantee::BestEffort);
        assert!(status.tier.label().contains("best-effort"));
        let rendered = serde_json::to_string(&status).unwrap();
        assert!(rendered.contains("best-effort"), "{rendered}");
        assert!(domain.selection().describe().contains("BEST-EFFORT"));
    }

    #[tokio::test]
    async fn tier1_escalates_to_sigkill_for_a_sigterm_trapping_child() {
        let signals = FakeSignals::new(vec![process(100, 100, true), process(101, 100, false)]);
        let domain = tier1_domain(signals.clone());
        let mut session_domain = domain.open_session(&session(), Generation(3)).unwrap();
        session_domain.set_leader(100).unwrap();
        session_domain.track(101).unwrap();

        let outcome = session_domain
            .terminate(Duration::from_millis(60))
            .await;

        assert!(outcome.converged(), "SIGKILL must finish the job");
        assert!(
            outcome.is_best_effort(),
            "tier 1 never claims a hard guarantee"
        );
        assert!(signals.alive_pids().is_empty());
        assert_eq!(signals.reaped_pids(), vec![100, 101], "no zombies left");
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
            KillDomainSelection::tier1(Tier2Unavailable::NotLinux),
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
            KillDomainSelection::tier1(Tier2Unavailable::NotLinux),
            TrackingLimits {
                max_tracked_per_session: 10,
                max_tracked_global: 1,
                reserved_fd_headroom: 0,
            },
            AuditLogger,
            FakeSignals::new(vec![]),
        );
        let mut a = global.open_session(&SessionName::parse("a").unwrap(), Generation(1)).unwrap();
        let mut b = global.open_session(&SessionName::parse("b").unwrap(), Generation(1)).unwrap();
        a.track(10).unwrap();
        assert!(matches!(b.track(11), Err(Error::CapacityExceeded { .. })));

        // Releasing a session returns its budget.
        a.release();
        b.track(11).unwrap();
    }

    // ---- Tier 2: exercised against a hand-written cgroup directory ----------

    struct FakeCgroupFs {
        root: PathBuf,
    }

    impl FakeCgroupFs {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "oab-pty-kd-{}-{name}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn session_dir(&self, session: &SessionName) -> PathBuf {
            self.root.join(format!("oab-pty-{session}"))
        }
    }

    impl Drop for FakeCgroupFs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn tier2_domain(base: PathBuf) -> KillDomain {
        KillDomain::with_signals(
            KillDomainSelection {
                tier: KillDomainTier::Tier2GuaranteedCgroup,
                tier2_unavailable: None,
                cgroup_base: Some(base),
            },
            TrackingLimits::default(),
            AuditLogger,
            FakeSignals::new(vec![]),
        )
    }

    #[test]
    fn membership_comes_from_cgroup_procs_and_tolerates_noise() {
        let fs = FakeCgroupFs::new("members");
        let name = session();
        let domain = tier2_domain(fs.root.clone());
        let session_domain = domain.open_session(&name, Generation(1)).unwrap();
        std::fs::write(fs.session_dir(&name).join("cgroup.procs"), "11\n\n12\nbogus\n0\n").unwrap();
        assert_eq!(session_domain.members().unwrap(), vec![11, 12]);
    }

    #[tokio::test]
    async fn tier2_converges_and_reports_a_hard_guarantee() {
        let fs = FakeCgroupFs::new("converge");
        let name = session();
        let domain = tier2_domain(fs.root.clone());
        let mut session_domain = domain.open_session(&name, Generation(1)).unwrap();
        let dir = fs.session_dir(&name);
        std::fs::write(dir.join("cgroup.procs"), "500\n501\n").unwrap();
        std::fs::write(dir.join("cgroup.kill"), "").unwrap();

        // Stand-in kernel: honours cgroup.kill by emptying the cgroup.
        let watcher_dir = dir.clone();
        let kernel = tokio::spawn(async move {
            loop {
                if std::fs::read_to_string(watcher_dir.join("cgroup.kill"))
                    .map(|content| content.trim() == "1")
                    .unwrap_or(false)
                {
                    std::fs::write(watcher_dir.join("cgroup.procs"), "").unwrap();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        });

        let outcome = session_domain.terminate(Duration::from_secs(5)).await;
        kernel.await.unwrap();
        assert!(outcome.converged());
        assert_eq!(outcome.guarantee, TeardownGuarantee::Hard);
        assert!(!outcome.is_best_effort());

        session_domain.release();
        // The fake cgroup's files are ordinary files, so `rmdir` would fail on
        // them; a real cgroupfs directory holds only kernel pseudo-files and is
        // removable once empty. Model that before asserting removal.
        let _ = std::fs::remove_file(dir.join("cgroup.procs"));
        let _ = std::fs::remove_file(dir.join("cgroup.kill"));
        let mut cgroup = SessionCgroup::create(&fs.root, &name).unwrap();
        cgroup.remove().unwrap();
        assert!(!dir.exists(), "the session cgroup is removed on release");
    }

    #[tokio::test]
    async fn tier2_reports_survivors_when_the_cgroup_never_empties() {
        let fs = FakeCgroupFs::new("stuck");
        let name = session();
        let domain = tier2_domain(fs.root.clone());
        let mut session_domain = domain.open_session(&name, Generation(1)).unwrap();
        let dir = fs.session_dir(&name);
        std::fs::write(dir.join("cgroup.procs"), "700\n").unwrap();
        std::fs::write(dir.join("cgroup.kill"), "").unwrap();

        // Bounded wait: teardown returns with the anomaly instead of hanging.
        let outcome = session_domain.terminate(Duration::from_secs(1)).await;
        assert_eq!(outcome.survivors, vec![700]);
        assert_eq!(domain.leaked_process_count(), 1);
    }

    #[test]
    fn tier2_preopens_cgroup_procs_for_the_pre_exec_write() {
        let fs = FakeCgroupFs::new("procsfd");
        let name = session();
        let domain = tier2_domain(fs.root.clone());
        let mut session_domain = domain.open_session(&name, Generation(1)).unwrap();
        std::fs::write(fs.session_dir(&name).join("cgroup.procs"), "").unwrap();
        #[cfg(unix)]
        {
            let fd = session_domain.procs_fd().unwrap();
            assert!(fd.is_some(), "tier 2 must hand the spawner a writable fd");
        }
        assert_eq!(session_domain.tier(), KillDomainTier::Tier2GuaranteedCgroup);
    }

    // ---- tier selection ----------------------------------------------------

    #[test]
    fn tier2_is_selected_only_with_delegation_and_a_pre_exec_spawner() {
        let selection = resolve_tier(
            KillDomainRequirement::Tier1Allowed,
            Ok(PathBuf::from("/sys/fs/cgroup")),
            SpawnCapability::PreExecCgroupJoin,
        )
        .unwrap();
        assert_eq!(selection.tier, KillDomainTier::Tier2GuaranteedCgroup);
        assert!(selection.tier2_unavailable.is_none());
        assert!(!selection.tier.is_best_effort());
    }

    #[test]
    fn missing_delegation_falls_back_to_tier1_with_the_distinguishing_reason() {
        for reason in [
            Tier2Unavailable::NoWritableCgroupMount,
            Tier2Unavailable::SubtreeNotDelegated,
            Tier2Unavailable::OutsideCgroupNamespace,
            Tier2Unavailable::SyscallBlocked("pidfd_open: ENOSYS".into()),
            Tier2Unavailable::NotLinux,
        ] {
            let selection = resolve_tier(
                KillDomainRequirement::Tier1Allowed,
                Err(reason.clone()),
                SpawnCapability::PreExecCgroupJoin,
            )
            .unwrap();
            assert_eq!(
                selection.tier,
                KillDomainTier::Tier1BestEffortProcessGroup
            );
            assert_eq!(selection.tier2_unavailable, Some(reason.clone()));
            let described = selection.describe();
            assert!(described.contains("BEST-EFFORT"), "{described}");
            assert!(
                described.contains(&reason.to_string()),
                "the reason must reach the operator: {described}"
            );
        }
    }

    #[test]
    fn tier2_required_is_fail_closed_when_delegation_is_absent() {
        let error = resolve_tier(
            KillDomainRequirement::Tier2Required,
            Err(Tier2Unavailable::SubtreeNotDelegated),
            SpawnCapability::PreExecCgroupJoin,
        )
        .expect_err("must never silently downgrade a requested guarantee");
        assert!(error.to_string().contains("tier2-required"));
    }

    #[test]
    fn a_spawner_without_pre_exec_never_claims_tier2() {
        // Delegation is present, but joining after `execv` would mean adversary
        // code ran outside its cgroup, so Tier 2 is refused either way: loud
        // downgrade when Tier 1 is allowed, hard failure when it is required.
        let downgraded = resolve_tier(
            KillDomainRequirement::Tier1Allowed,
            Ok(PathBuf::from("/sys/fs/cgroup")),
            SpawnCapability::ProcessGroupOnly,
        )
        .unwrap();
        assert_eq!(
            downgraded.tier2_unavailable,
            Some(Tier2Unavailable::SpawnerCannotJoinCgroupPreExec)
        );
        assert!(downgraded.tier.is_best_effort());

        assert!(resolve_tier(
            KillDomainRequirement::Tier2Required,
            Ok(PathBuf::from("/sys/fs/cgroup")),
            SpawnCapability::ProcessGroupOnly,
        )
        .is_err());
    }

    #[test]
    fn pid_formatting_for_the_pre_exec_hook_is_allocation_free_and_exact() {
        let mut buf = [0u8; 24];
        for (pid, expected) in [(1, "1"), (7, "7"), (65535, "65535"), (2_147_483_647, "2147483647")]
        {
            let len = format_pid_decimal(pid, &mut buf);
            assert_eq!(&buf[..len], expected.as_bytes());
        }
        let len = format_pid_decimal(0, &mut buf);
        assert_eq!(&buf[..len], b"0");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn non_linux_never_claims_containment() {
        assert_eq!(probe_tier2(), Err(Tier2Unavailable::NotLinux));
        assert_eq!(establish_subreaper(), SubreaperStatus::Unsupported);
        let selection = detect(
            KillDomainRequirement::Tier1Allowed,
            SpawnCapability::PreExecCgroupJoin,
        )
        .unwrap();
        assert!(selection.tier.is_best_effort());
        assert!(detect(
            KillDomainRequirement::Tier2Required,
            SpawnCapability::PreExecCgroupJoin
        )
        .is_err());
    }

    // ---- integration: real processes and real cgroups -----------------------
    // Ignored by default. These are the only tests here that leave the process.

    #[tokio::test]
    #[ignore = "spawns real processes"]
    async fn tier1_teardown_against_a_real_process_group() {
        let domain = KillDomain::new(
            KillDomainSelection::tier1(Tier2Unavailable::NotLinux),
            TrackingLimits::default(),
            AuditLogger,
        );
        let mut session_domain = domain
            .open_session(&SessionName::parse("real-tier1").unwrap(), Generation(1))
            .unwrap();
        let child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .spawn()
            .expect("spawn a real child");
        let pid = child.id() as i32;
        session_domain.set_leader(pid).unwrap();

        let outcome = session_domain.terminate(Duration::from_secs(2)).await;
        assert!(outcome.converged(), "survivors: {:?}", outcome.survivors);
        assert!(outcome.is_best_effort());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "touches cgroups; needs cgroup v2 delegation to this container"]
    async fn tier2_probe_and_teardown_against_the_real_kernel() {
        let base = match probe_tier2() {
            Ok(base) => base,
            Err(reason) => panic!("tier 2 unavailable here: {reason}"),
        };
        let selection = resolve_tier(
            KillDomainRequirement::Tier2Required,
            Ok(base),
            SpawnCapability::PreExecCgroupJoin,
        )
        .unwrap();
        assert_eq!(selection.tier, KillDomainTier::Tier2GuaranteedCgroup);

        let domain = KillDomain::new(selection, TrackingLimits::default(), AuditLogger);
        let mut session_domain = domain
            .open_session(&SessionName::parse("real-tier2").unwrap(), Generation(1))
            .unwrap();
        let procs_fd = session_domain.procs_fd().unwrap().expect("tier 2 fd");

        // Join before exec, exactly as the spawner must: the child writes its own
        // pid through the inherited descriptor before it becomes the target
        // program, so adversary code never runs outside its cgroup.
        let child = unsafe {
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg("exec sleep 30")
                .pre_exec(move || preexec_join_cgroup(procs_fd))
                .spawn()
                .expect("spawn")
        };
        let pid = child.id() as i32;
        session_domain.set_leader(pid).unwrap();
        assert!(
            session_domain.members().unwrap().contains(&pid),
            "membership must come from cgroup.procs"
        );

        let outcome = session_domain.terminate(Duration::from_secs(2)).await;
        assert_eq!(outcome.guarantee, TeardownGuarantee::Hard);
        assert!(outcome.converged(), "cgroup.kill must leave zero survivors");
        session_domain.release();
    }
}

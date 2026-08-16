//! The PTY session manager: named byte-stream sessions.
//!
//! This is deliberately **not** the ACP pool's turn-based model. An ACP session
//! is a request/response conversation; a PTY session is an unbounded byte stream
//! with its own liveness definition (bytes and sockets, not turns), so nothing
//! here is shared with `openab-core`'s pool.
//!
//! Three rules govern the concurrency design, and every one of them exists
//! because getting it wrong is a security or availability bug rather than a
//! performance one:
//!
//! 1. **Total lock order.** Revocation (generation bump + stored-hash deletion)
//!    → attach CAS → replay registration, in that order under the session state
//!    lock; where the buffer lock is also needed, the session lock is taken
//!    first and never the reverse. Nested locks follow
//!    `state → subscriber → buffer`, and the [`crate::token::TokenStore`]'s
//!    internal lock is only ever taken *inside* the state lock, never outside.
//! 2. **The session lock covers state, never I/O.** Socket and PTY I/O happens
//!    after the guard is dropped, fenced by generation so stale work is ignored.
//!    Kill and TTL paths therefore cannot be blocked by a per-connection drain:
//!    a slow or malicious client can never delay renew, expiry, or teardown.
//! 3. **Copy values out; drop guards before doing anything else.** A real bug
//!    already found in this crate: in edition 2021 an `if let` scrutinee holding
//!    a `parking_lot` guard keeps that guard alive for the *whole* statement, so
//!    a body that re-locks self-deadlocks against a non-reentrant mutex.
//!
//! The I/O threads never take the state lock. They read the fence and activity
//! clock from [`SessionIo`]'s atomics, which are written under the state lock —
//! the lock stays authoritative, the atomics are its I/O-side mirror.

use crate::audit::{AuditEvent, AuditKind, AuditLogger, TerminationClass};
use crate::close_code;
use crate::config::PtyConfig;
use crate::killdomain::{KillDomain, KillDomainTier, SessionKillDomain, TeardownOutcome};
use crate::ringbuf::{ByteQueue, Push, QueueBounds, Replay, RingBuffer};
use crate::termfilter::TermFilter;
use crate::token::{MintedAttachToken, TokenStore};
use crate::{Error, Generation, SessionName};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;

/// Environment variables a PTY child may inherit. Everything else is dropped:
/// `OPENAB_*`, cloud credentials, and platform tokens must never reach a shell
/// the operator handed to a human.
pub const ENV_ALLOWLIST: &[&str] = &["TERM", "LANG", "PATH", "HOME", "USER", "SHELL"];
/// Locale variables are allowlisted by prefix (`LC_ALL`, `LC_CTYPE`, ...).
pub const ENV_ALLOWLIST_PREFIXES: &[&str] = &["LC_"];

const DEFAULT_TERM: &str = "xterm-256color";
/// Read chunk from the PTY master.
const READ_CHUNK: usize = 16 * 1024;
/// Number of input chunks that may sit between the socket and the PTY master
/// before the client is considered unable to keep up (fail closed).
const INPUT_QUEUE_CHUNKS: usize = 64;
/// Bounded tombstone table so reattach-to-dead keeps working without unbounded
/// growth.
const MAX_TOMBSTONES: usize = 32;

/// Per-connection identity. Distinct from [`Generation`], which fences *tokens*:
/// this fences *connections* inside one generation, and the two must not be
/// conflated (a takeover changes only this one).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ConnGeneration(pub u64);

/// Liveness, TTL, and abuse-control knobs.
///
/// These are not in the delivered `[pty]` projection yet — `config.rs` rejects
/// unknown keys, and it is outside this change — so the ADR's recommended
/// defaults live here and the integrator can override them programmatically.
#[derive(Clone, Copy, Debug)]
pub struct SessionPolicy {
    /// Detached-idle TTL.
    pub detached_idle_ttl: Duration,
    /// Absolute lifetime cap; applies even while attached, so an open browser
    /// tab cannot pin capacity forever.
    pub absolute_ttl: Duration,
    /// WS ping interval (ADR band: 15–30s).
    pub ping_interval: Duration,
    /// Missed pings before a half-open socket counts as detached (ADR band: 2–3).
    pub missed_pings_before_detached: u32,
    /// How long before forced teardown the client-visible warning is sent.
    pub ttl_warning_lead: Duration,
    /// Takeovers allowed per window, per session generation.
    pub max_takeovers_per_window: u32,
    pub takeover_window: Duration,
    /// `SIGTERM`→`SIGKILL` grace handed to the kill domain.
    pub teardown_grace: Duration,
}

impl Default for SessionPolicy {
    fn default() -> Self {
        Self {
            detached_idle_ttl: Duration::from_secs(30 * 60),
            absolute_ttl: Duration::from_secs(12 * 60 * 60),
            ping_interval: Duration::from_secs(20),
            missed_pings_before_detached: 3,
            ttl_warning_lead: Duration::from_secs(30),
            max_takeovers_per_window: 3,
            takeover_window: Duration::from_secs(60),
            teardown_grace: Duration::from_secs(5),
        }
    }
}

impl SessionPolicy {
    /// Take the absolute TTL from the delivered projection, keep ADR defaults
    /// for the rest.
    pub fn from_config(config: &PtyConfig) -> Self {
        Self {
            absolute_ttl: config.absolute_session_ttl,
            ..Self::default()
        }
    }

    /// Reject values outside the ADR's stated bands rather than silently
    /// serving a liveness model nobody reviewed.
    pub fn validate(&self) -> Result<(), Error> {
        if self.ping_interval < Duration::from_secs(15) || self.ping_interval > Duration::from_secs(30)
        {
            return Err(Error::Config(
                "ping_interval must be between 15s and 30s".into(),
            ));
        }
        if !(2..=3).contains(&self.missed_pings_before_detached) {
            return Err(Error::Config(
                "missed_pings_before_detached must be 2 or 3".into(),
            ));
        }
        if self.detached_idle_ttl.is_zero() || self.absolute_ttl.is_zero() {
            return Err(Error::Config("TTLs must be non-zero".into()));
        }
        if self.max_takeovers_per_window == 0 || self.takeover_window.is_zero() {
            return Err(Error::Config(
                "takeover rate limit must permit at least one takeover per window".into(),
            ));
        }
        Ok(())
    }
}

/// Terminal size, propagated to the child with `TIOCSWINSZ`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct WindowSize {
    pub rows: u16,
    pub cols: u16,
}

impl Default for WindowSize {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

impl WindowSize {
    /// Bounds-checked per the strict control-frame allowlist.
    pub fn validated(rows: u16, cols: u16) -> Result<Self, Error> {
        if !(1..=1000).contains(&rows) || !(1..=1000).contains(&cols) {
            return Err(Error::Other(format!(
                "resize out of bounds: {rows}x{cols} (1..=1000)"
            )));
        }
        Ok(Self { rows, cols })
    }
}

/// Build the child's environment from an allowlist.
pub fn child_env<I>(source: I, home: &str, size_term: Option<&str>) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut env: Vec<(String, String)> = source
        .into_iter()
        .filter(|(key, _)| {
            ENV_ALLOWLIST.contains(&key.as_str())
                || ENV_ALLOWLIST_PREFIXES
                    .iter()
                    .any(|prefix| key.starts_with(prefix))
        })
        .collect();
    env.sort();
    env.dedup_by(|a, b| a.0 == b.0);
    if !env.iter().any(|(key, _)| key == "TERM") {
        env.push((
            "TERM".to_string(),
            size_term.unwrap_or(DEFAULT_TERM).to_string(),
        ));
    }
    if !env.iter().any(|(key, _)| key == "HOME") {
        env.push(("HOME".to_string(), home.to_string()));
    }
    env
}

// ---------------------------------------------------------------------------
// Spawner seam
// ---------------------------------------------------------------------------

/// What the manager asks the spawner for. The command comes from operator
/// configuration; there is no path by which a client supplies it.
#[derive(Clone, Debug)]
pub struct SpawnRequest {
    pub session: SessionName,
    pub command: PathBuf,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub size: WindowSize,
}

/// Resize and exit-status access for a spawned PTY.
pub trait PtyControl: Send + Sync {
    fn resize(&self, size: WindowSize) -> Result<(), Error>;
    /// Non-blocking: `Some(code)` once the child has exited.
    fn try_wait(&self) -> Result<Option<i32>, Error>;
}

/// A spawned PTY plus its master ends.
pub struct SpawnedPty {
    pub pid: Option<i32>,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    pub control: Arc<dyn PtyControl>,
}

/// Spawning seam. Implemented for real by [`PortablePtySpawner`]; tests use a
/// hand-written fake so no unit test creates a process.
pub trait PtySpawner: Send + Sync {
    fn spawn(&self, request: SpawnRequest, containment: &mut SessionKillDomain)
        -> Result<SpawnedPty, Error>;
    /// What the spawner can do at fork time, which decides whether Tier 2 is
    /// selectable at all.
    fn capability(&self) -> crate::killdomain::SpawnCapability;
}

/// The real spawner.
///
/// `portable-pty` owns its own pre-`exec` closure (it calls `setsid`, sets the
/// controlling terminal, and closes every descriptor above 2). That gives us the
/// per-session process group Tier 1 needs for free — the child is a session and
/// group leader, so `pgid == pid` — but it also means no caller-supplied
/// pre-`execv` hook can run, so this spawner reports
/// [`SpawnCapability::ProcessGroupOnly`] and Tier 2 is never claimed on top of
/// it.
///
/// [`SpawnCapability::ProcessGroupOnly`]: crate::killdomain::SpawnCapability::ProcessGroupOnly
#[derive(Debug, Default, Clone, Copy)]
pub struct PortablePtySpawner;

impl PtySpawner for PortablePtySpawner {
    fn spawn(
        &self,
        request: SpawnRequest,
        containment: &mut SessionKillDomain,
    ) -> Result<SpawnedPty, Error> {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};

        if containment.tier() == KillDomainTier::Tier2GuaranteedCgroup {
            // Fail closed rather than migrate after `exec`: a post-`exec` write
            // to cgroup.procs would mean adversary code already ran outside its
            // cgroup, which is precisely the window Tier 2 exists to close.
            return Err(Error::Other(
                "tier 2 containment requires a pre-execv capable spawner; \
                 portable-pty owns its pre-exec closure"
                    .into(),
            ));
        }

        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: request.size.rows,
                cols: request.size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| Error::Other(format!("openpty failed: {error}")))?;

        let mut command = CommandBuilder::new(request.command.as_os_str());
        command.env_clear();
        for (key, value) in &request.env {
            command.env(key, value);
        }
        command.cwd(&request.cwd);

        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| Error::Other(format!("spawn failed: {error}")))?;
        let pid = child.process_id().and_then(|pid| i32::try_from(pid).ok());

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| Error::Other(format!("clone pty reader failed: {error}")))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| Error::Other(format!("take pty writer failed: {error}")))?;

        if let Some(pid) = pid {
            // portable-pty called setsid, so the child leads its own process
            // group: pgid == pid is Tier 1's signal path.
            containment.set_leader(pid)?;
        }

        Ok(SpawnedPty {
            pid,
            reader,
            writer,
            control: Arc::new(PortablePtyControl {
                master: Mutex::new(pair.master),
                child: Mutex::new(child),
            }),
        })
    }

    fn capability(&self) -> crate::killdomain::SpawnCapability {
        crate::killdomain::SpawnCapability::ProcessGroupOnly
    }
}

struct PortablePtyControl {
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
}

impl PtyControl for PortablePtyControl {
    fn resize(&self, size: WindowSize) -> Result<(), Error> {
        self.master
            .lock()
            .resize(portable_pty::PtySize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| Error::Other(format!("TIOCSWINSZ failed: {error}")))
    }

    fn try_wait(&self) -> Result<Option<i32>, Error> {
        let status = self.child.lock().try_wait()?;
        Ok(status.map(|status| status.exit_code() as i32))
    }
}

// ---------------------------------------------------------------------------
// Outbound path (server → client)
// ---------------------------------------------------------------------------

/// Text control frames the server must forward. Bytes travel as binary frames.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "type")]
pub enum ControlEvent {
    /// Sent once at attach: the workspace is ephemeral, and Tier 1 teardown is
    /// best-effort. Both are stated rather than left to be discovered.
    AttachNotice {
        ephemeral_workspace: bool,
        teardown_best_effort: bool,
        externalise_with: &'static str,
    },
    /// Bytes were dropped; the client should clear and redraw.
    Gap { bytes_dropped: u64 },
    /// A TTL is about to force teardown.
    TtlWarning {
        seconds_remaining: u64,
        absolute: bool,
    },
    /// First-class "workspace volume full" rather than an opaque write error
    /// inside the terminal.
    WorkspaceVolumeFull { used_kib: u64, budget_kib: u64 },
}

/// One item for the socket writer task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutboundItem {
    Bytes(Vec<u8>),
    Control(ControlEvent),
    Close(u16),
}

/// A connection's outbound queue: bounded backlog, coalesced gap reporting, and
/// a close code that can be set without waiting for the client to drain.
#[derive(Debug)]
pub struct Outbound {
    conn: ConnGeneration,
    queue: Mutex<ByteQueue>,
    controls: Mutex<VecDeque<ControlEvent>>,
    close_code: Mutex<Option<u16>>,
    notify: Notify,
}

impl Outbound {
    fn new(conn: ConnGeneration, bounds: QueueBounds) -> Arc<Self> {
        Arc::new(Self {
            conn,
            queue: Mutex::new(ByteQueue::new(bounds)),
            controls: Mutex::new(VecDeque::new()),
            close_code: Mutex::new(None),
            notify: Notify::new(),
        })
    }

    pub fn conn(&self) -> ConnGeneration {
        self.conn
    }

    /// Enqueue PTY bytes. Never blocks and never grows without bound: past the
    /// hard watermark the connection is closed with `SLOW_CLIENT`.
    fn push_bytes(&self, bytes: &[u8]) {
        let outcome = self.queue.lock().push(bytes);
        if let Push::HardWatermark { .. } = outcome {
            self.close(close_code::SLOW_CLIENT);
            return;
        }
        self.notify.notify_one();
    }

    fn push_control(&self, event: ControlEvent) {
        self.controls.lock().push_back(event);
        self.notify.notify_one();
    }

    /// Terminate this connection's write path. Idempotent, non-blocking, and
    /// deliberately independent of whether the client ever reads again.
    pub fn close(&self, code: u16) {
        {
            let mut current = self.close_code.lock();
            if current.is_none() {
                *current = Some(code);
            }
        }
        self.notify.notify_waiters();
        self.notify.notify_one();
    }

    pub fn close_code(&self) -> Option<u16> {
        *self.close_code.lock()
    }

    /// Non-blocking drain step, in priority order: pending gap, control frames,
    /// bytes, then the close code.
    pub fn try_next(&self) -> Option<OutboundItem> {
        if let Some(dropped) = self.queue.lock().take_unreported_gap() {
            return Some(OutboundItem::Control(ControlEvent::Gap {
                bytes_dropped: dropped,
            }));
        }
        let control = self.controls.lock().pop_front();
        if let Some(control) = control {
            return Some(OutboundItem::Control(control));
        }
        let bytes = self.queue.lock().take(READ_CHUNK);
        if !bytes.is_empty() {
            return Some(OutboundItem::Bytes(bytes));
        }
        self.close_code().map(OutboundItem::Close)
    }

    /// Await the next item. Returns `None` only when the connection is closed
    /// and fully drained, so a server writer loop terminates deterministically.
    pub async fn next(&self) -> Option<OutboundItem> {
        loop {
            if let Some(item) = self.try_next() {
                if matches!(item, OutboundItem::Close(_)) {
                    return Some(item);
                }
                return Some(item);
            }
            if self.close_code().is_some() {
                return None;
            }
            self.notify.notified().await;
        }
    }
}

/// Why a client write was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    /// This connection lost the CAS (takeover) or the session ended. Its write
    /// path is dropped before its socket closes, so this is observable first.
    Evicted,
    /// The client exceeded the input watermark toward the PTY master. Fail
    /// closed: the server disconnects rather than growing a queue.
    Backpressure,
}

struct InputChunk {
    conn: ConnGeneration,
    bytes: Vec<u8>,
}

/// Shared I/O-side view of a session. The state lock is authoritative; these
/// atomics are its mirror so reader/writer threads never take that lock.
#[derive(Debug)]
struct SessionIo {
    /// Connection fence. The writer thread honours only this value.
    owner_conn: AtomicU64,
    /// Millis since the manager epoch. Written by both directions of traffic.
    last_activity_ms: AtomicU64,
    /// Current subscriber, if any. Its own lock, held only for the swap.
    subscriber: Mutex<Option<Arc<Outbound>>>,
    /// Retained scrollback. Always locked *after* the state lock.
    buffer: Mutex<RingBuffer>,
    alive: AtomicBool,
}

impl SessionIo {
    fn touch(&self, epoch: Instant) {
        self.last_activity_ms.store(
            Instant::now().duration_since(epoch).as_millis() as u64,
            Ordering::Relaxed,
        );
    }

    fn subscriber_for(&self, conn: ConnGeneration) -> Option<Arc<Outbound>> {
        // Copy the Arc out and drop the guard on this line: holding a
        // parking_lot guard into an `if let` body is the self-deadlock this
        // crate already hit once.
        let current = self.subscriber.lock().clone();
        current.filter(|outbound| outbound.conn == conn)
    }
}

/// Per-session takeover budget, scoped to the session generation.
///
/// The scoping is the security property: a thief who exhausts the bucket must
/// not be able to lock the victim out of the `session renew` recovery path, so a
/// generation bump resets the bucket and the first attach under a new generation
/// always passes.
#[derive(Debug)]
struct TakeoverLimiter {
    generation: Generation,
    window_start: Instant,
    count: u32,
    first_attach_of_generation: bool,
}

impl TakeoverLimiter {
    fn new(generation: Generation, now: Instant) -> Self {
        Self {
            generation,
            window_start: now,
            count: 0,
            first_attach_of_generation: true,
        }
    }

    fn allow(&mut self, generation: Generation, now: Instant, policy: &SessionPolicy) -> bool {
        if self.generation != generation {
            *self = Self::new(generation, now);
        }
        if self.first_attach_of_generation {
            self.first_attach_of_generation = false;
            return true;
        }
        if now.duration_since(self.window_start) >= policy.takeover_window {
            self.window_start = now;
            self.count = 0;
        }
        if self.count >= policy.max_takeovers_per_window {
            return false;
        }
        self.count += 1;
        true
    }
}

/// State protected by the session lock. State only — never I/O.
#[derive(Debug)]
struct SessionState {
    generation: Generation,
    owner_conn: ConnGeneration,
    next_conn: u64,
    created_at: Instant,
    attached_since: Option<Instant>,
    missed_pings: u32,
    alive: bool,
    termination: Option<TerminationClass>,
    warned_absolute: bool,
    warned_idle: bool,
    size: WindowSize,
    takeovers: TakeoverLimiter,
    disk_used_kib: u64,
}

/// One live session.
pub struct Session {
    name: SessionName,
    state: Mutex<SessionState>,
    io: Arc<SessionIo>,
    input: tokio::sync::mpsc::Sender<InputChunk>,
    control: Arc<dyn PtyControl>,
    /// Never locked while the state lock is held: teardown must not be able to
    /// deadlock against attach.
    kill: tokio::sync::Mutex<SessionKillDomain>,
    filter: Mutex<TermFilter>,
    backlog_bounds: QueueBounds,
    epoch: Instant,
    tier_best_effort: bool,
    /// Copied from the policy so the ping path needs no manager reference.
    missed_ping_limit: u32,
}

/// A client's handle on an attached connection.
pub struct AttachedConnection {
    pub session: SessionName,
    pub generation: Generation,
    pub conn: ConnGeneration,
    pub outbound: Arc<Outbound>,
    session_handle: Arc<Session>,
}

impl AttachedConnection {
    /// Client → PTY. Capability responses are stripped here, at the single
    /// chokepoint, so every emulator is protected rather than each being asked
    /// to behave.
    pub fn write_input(&self, bytes: &[u8]) -> Result<(), InputError> {
        self.session_handle.write_input(self.conn, bytes)
    }

    /// TIOCSWINSZ propagation for a `resize` control frame.
    pub fn resize(&self, size: WindowSize) -> Result<(), Error> {
        self.session_handle.resize(self.conn, size)
    }

    /// A pong resets the missed-ping counter and counts as liveness.
    pub fn on_pong(&self) {
        self.session_handle.on_pong(self.conn);
    }

    /// A missed ping. Returns `true` once the socket counts as detached, at
    /// which point the server should stop treating it as attached.
    pub fn on_missed_ping(&self) -> bool {
        self.session_handle.on_missed_ping(self.conn)
    }

    /// Voluntary detach (`detach` control frame or socket close).
    pub fn detach(&self) {
        self.session_handle.detach(self.conn);
    }
}

impl Session {
    fn write_input(&self, conn: ConnGeneration, bytes: &[u8]) -> Result<(), InputError> {
        // Fence first, against the atomic mirror: no state lock on the I/O path.
        if !self.io.alive.load(Ordering::Acquire)
            || self.io.owner_conn.load(Ordering::Acquire) != conn.0
        {
            return Err(InputError::Evicted);
        }
        let filtered = {
            let mut filter = self.filter.lock();
            filter.filter(bytes).to_vec()
        };
        if filtered.is_empty() {
            return Ok(());
        }
        self.input
            .try_send(InputChunk {
                conn,
                bytes: filtered,
            })
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => InputError::Backpressure,
                tokio::sync::mpsc::error::TrySendError::Closed(_) => InputError::Evicted,
            })?;
        self.io.touch(self.epoch);
        Ok(())
    }

    fn resize(&self, conn: ConnGeneration, size: WindowSize) -> Result<(), Error> {
        if self.io.owner_conn.load(Ordering::Acquire) != conn.0 {
            return Err(Error::Unauthorized);
        }
        self.state.lock().size = size;
        self.control.resize(size)
    }

    fn on_pong(&self, conn: ConnGeneration) {
        let mut state = self.state.lock();
        if state.owner_conn == conn {
            state.missed_pings = 0;
        }
        drop(state);
        self.io.touch(self.epoch);
    }

    fn on_missed_ping(&self, conn: ConnGeneration) -> bool {
        // Copy the decision out under the lock, act on it after dropping it.
        let (detached, outbound) = {
            let mut state = self.state.lock();
            if state.owner_conn != conn {
                return true;
            }
            state.missed_pings += 1;
            if state.missed_pings < self.policy_missed_pings() {
                return false;
            }
            state.attached_since = None;
            state.owner_conn = ConnGeneration(0);
            self.io.owner_conn.store(0, Ordering::Release);
            (true, self.io.subscriber.lock().take())
        };
        if let Some(outbound) = outbound {
            // Connection-evict, not session-teardown: the process survives a
            // half-open socket.
            outbound.close(close_code::TAKEOVER);
        }
        detached
    }

    fn policy_missed_pings(&self) -> u32 {
        self.missed_ping_limit
    }

    fn detach(&self, conn: ConnGeneration) {
        let outbound = {
            let mut state = self.state.lock();
            if state.owner_conn != conn {
                return;
            }
            state.owner_conn = ConnGeneration(0);
            state.attached_since = None;
            state.missed_pings = 0;
            self.io.owner_conn.store(0, Ordering::Release);
            self.io.subscriber.lock().take()
        };
        drop(outbound);
    }

    pub fn name(&self) -> &SessionName {
        &self.name
    }

    pub fn generation(&self) -> Generation {
        self.state.lock().generation
    }

    pub fn is_alive(&self) -> bool {
        self.io.alive.load(Ordering::Acquire)
    }

    pub fn is_attached(&self) -> bool {
        self.io.owner_conn.load(Ordering::Acquire) != 0
    }

    pub fn window_size(&self) -> WindowSize {
        self.state.lock().size
    }

    pub fn total_written(&self) -> u64 {
        self.io.buffer.lock().total_written()
    }
}

/// Snapshot for admin `list` output.
#[derive(Clone, Debug, Serialize)]
pub struct SessionStatus {
    pub name: String,
    pub generation: u64,
    pub alive: bool,
    pub attached: bool,
    pub bytes_written: u64,
    pub disk_used_kib: u64,
    pub tier: KillDomainTier,
    /// Repeated per session because the guarantee is per-runtime but consumed
    /// per session.
    pub teardown_best_effort: bool,
    pub absolute_ttl_best_effort: bool,
}

/// Counters the integrator exposes as metrics.
#[derive(Debug, Default)]
pub struct Metrics {
    pub sessions_created: AtomicU64,
    pub admission_rejected: AtomicU64,
    pub takeovers: AtomicU64,
    pub takeovers_rate_limited: AtomicU64,
    pub ttl_expired: AtomicU64,
    pub self_exits: AtomicU64,
    pub workspace_full: AtomicU64,
}

impl Metrics {
    fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// Result of a create/renew/restart: the one-time attach token plus the fence it
/// is bound to.
pub struct SessionCredential {
    pub session: SessionName,
    pub generation: Generation,
    pub token: MintedAttachToken,
}

/// Why an attach was refused, with the close code the server should use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachRejection {
    /// Preempt budget exhausted for this generation.
    TakeoverRateLimited,
    /// Generation fence mismatch: the token belongs to a superseded generation.
    StaleGeneration,
}

impl AttachRejection {
    pub fn close_code(self) -> u16 {
        match self {
            Self::TakeoverRateLimited => close_code::CAPACITY,
            Self::StaleGeneration => close_code::RENEWED,
        }
    }
}

/// Report from a teardown, for audit and status.
#[derive(Clone, Debug, Serialize)]
pub struct TeardownReport {
    pub session: String,
    pub generation: u64,
    pub class: TerminationClass,
    pub close_code: u16,
    pub kill_domain: TeardownOutcome,
}

struct Tombstone {
    generation: Generation,
    class: TerminationClass,
}

struct ManagerState {
    sessions: BTreeMap<SessionName, Arc<Session>>,
    /// Names of ended sessions, so reattach-to-dead is a distinct error offering
    /// restart-in-place instead of an indistinguishable "not found".
    tombstones: VecDeque<(SessionName, Tombstone)>,
}

/// The session manager.
pub struct SessionManager {
    state: Mutex<ManagerState>,
    config: PtyConfig,
    policy: SessionPolicy,
    tokens: TokenStore,
    audit: AuditLogger,
    kill: Arc<KillDomain>,
    spawner: Arc<dyn PtySpawner>,
    metrics: Metrics,
    epoch: Instant,
    /// Workspace usage reported by the integrator, in KiB.
    volume_used_kib: AtomicU64,
}

impl SessionManager {
    pub fn new(
        config: PtyConfig,
        policy: SessionPolicy,
        tokens: TokenStore,
        audit: AuditLogger,
        kill: Arc<KillDomain>,
        spawner: Arc<dyn PtySpawner>,
    ) -> Result<Arc<Self>, Error> {
        policy.validate()?;
        Ok(Arc::new(Self {
            state: Mutex::new(ManagerState {
                sessions: BTreeMap::new(),
                tombstones: VecDeque::new(),
            }),
            config,
            policy,
            tokens,
            audit,
            kill,
            spawner,
            metrics: Metrics::default(),
            epoch: Instant::now(),
            volume_used_kib: AtomicU64::new(0),
        }))
    }

    pub fn policy(&self) -> &SessionPolicy {
        &self.policy
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    pub fn kill_domain(&self) -> &Arc<KillDomain> {
        &self.kill
    }

    /// Admission-relevant workspace usage, reported by the integrator (the
    /// runtime does not stat the volume itself).
    pub fn report_volume_usage(&self, used_kib: u64) {
        self.volume_used_kib.store(used_kib, Ordering::Relaxed);
    }

    fn volume_full(&self) -> bool {
        let used = self.volume_used_kib.load(Ordering::Relaxed);
        used >= self.config.volume_capacity_kib as u64
    }

    pub fn list(&self) -> Vec<SessionStatus> {
        let sessions: Vec<Arc<Session>> = self.state.lock().sessions.values().cloned().collect();
        sessions
            .into_iter()
            .map(|session| {
                let state = session.state.lock();
                SessionStatus {
                    name: session.name.as_str().to_owned(),
                    generation: state.generation.0,
                    alive: session.io.alive.load(Ordering::Acquire),
                    attached: state.owner_conn != ConnGeneration(0),
                    bytes_written: session.io.buffer.lock().total_written(),
                    disk_used_kib: state.disk_used_kib,
                    tier: self.kill.tier(),
                    teardown_best_effort: self.kill.tier().is_best_effort(),
                    absolute_ttl_best_effort: self.kill.tier().is_best_effort(),
                }
            })
            .collect()
    }

    pub fn get(&self, name: &SessionName) -> Option<Arc<Session>> {
        self.state.lock().sessions.get(name).cloned()
    }

    /// Remote-admin `create`.
    ///
    /// Must be called from within a Tokio runtime: the session's output pump is
    /// a spawned task, while the PTY reader and writer are dedicated OS threads
    /// so a blocking `read`/`write` on the master can never occupy a runtime
    /// worker.
    pub fn create(
        self: &Arc<Self>,
        name: SessionName,
        size: WindowSize,
    ) -> Result<SessionCredential, Error> {
        {
            let state = self.state.lock();
            if state.sessions.contains_key(&name) {
                return Err(Error::AlreadyExists(name));
            }
            let live = state
                .sessions
                .values()
                .filter(|session| session.io.alive.load(Ordering::Acquire))
                .count();
            if live >= self.config.max_sessions {
                Metrics::bump(&self.metrics.admission_rejected);
                return Err(Error::CapacityExceeded {
                    limit: self.config.max_sessions,
                });
            }
        }
        if self.volume_full() {
            Metrics::bump(&self.metrics.admission_rejected);
            Metrics::bump(&self.metrics.workspace_full);
            return Err(Error::Other(
                "workspace volume full: refusing to create a session".into(),
            ));
        }
        self.spawn_session(name, size, Generation(1))
    }

    /// Restart-in-place: same name, fresh process, new generation, empty
    /// scrollback. Old tokens are invalid because the generation changed.
    pub async fn restart_in_place(
        self: &Arc<Self>,
        name: &SessionName,
    ) -> Result<SessionCredential, Error> {
        let existing = self.get(name);
        let next_generation = match existing {
            Some(session) => {
                let generation = session.generation();
                if session.is_alive() {
                    self.teardown(&session, TerminationClass::UserKill, close_code::SESSION_ENDED)
                        .await;
                }
                self.forget(name);
                Generation(generation.0 + 1)
            }
            None => {
                let tombstone = self.take_tombstone(name);
                match tombstone {
                    Some(dead) => Generation(dead.generation.0 + 1),
                    None => return Err(Error::NotFound(name.clone())),
                }
            }
        };
        let size = WindowSize::default();
        let credential = self.spawn_session(name.clone(), size, next_generation)?;
        self.audit.record(
            AuditEvent::new(AuditKind::SessionRestart)
                .session(name, credential.generation)
                .detail("restart_in_place: fresh process, empty scrollback"),
        );
        Ok(credential)
    }

    fn spawn_session(
        self: &Arc<Self>,
        name: SessionName,
        size: WindowSize,
        generation: Generation,
    ) -> Result<SessionCredential, Error> {
        let mut containment = self.kill.open_session(&name, generation)?;
        let env = child_env(
            std::env::vars(),
            &self.workspace().to_string_lossy(),
            Some(DEFAULT_TERM),
        );
        let request = SpawnRequest {
            session: name.clone(),
            command: self.config.command.clone(),
            cwd: self.workspace(),
            env,
            size,
        };
        let spawned = match self.spawner.spawn(request, &mut containment) {
            Ok(spawned) => spawned,
            Err(error) => {
                containment.release();
                return Err(error);
            }
        };

        let io = Arc::new(SessionIo {
            owner_conn: AtomicU64::new(0),
            last_activity_ms: AtomicU64::new(0),
            subscriber: Mutex::new(None),
            buffer: Mutex::new(RingBuffer::with_scrollback_kib(self.config.scrollback_kib)),
            alive: AtomicBool::new(true),
        });
        let (input_tx, input_rx) = tokio::sync::mpsc::channel::<InputChunk>(INPUT_QUEUE_CHUNKS);
        let session = Arc::new(Session {
            name: name.clone(),
            state: Mutex::new(SessionState {
                generation,
                owner_conn: ConnGeneration(0),
                next_conn: 1,
                created_at: Instant::now(),
                attached_since: None,
                missed_pings: 0,
                alive: true,
                termination: None,
                warned_absolute: false,
                warned_idle: false,
                size,
                takeovers: TakeoverLimiter::new(generation, Instant::now()),
                disk_used_kib: 0,
            }),
            io: io.clone(),
            input: input_tx,
            control: spawned.control.clone(),
            kill: tokio::sync::Mutex::new(containment),
            filter: Mutex::new(TermFilter::new()),
            backlog_bounds: QueueBounds::new(
                self.config.outbound_backlog_kib.saturating_mul(1024),
                Some(self.config.outbound_backlog_kib.saturating_mul(1024) as u64 * 4),
            ),
            epoch: self.epoch,
            tier_best_effort: self.kill.tier().is_best_effort(),
            missed_ping_limit: self.policy.missed_pings_before_detached,
        });

        spawn_writer_thread(io.clone(), input_rx, spawned.writer);
        spawn_reader_thread(self.clone(), session.clone(), spawned.reader);

        let token = self.tokens.mint_for_session(name.clone(), generation)?;
        self.state.lock().sessions.insert(name.clone(), session);
        Metrics::bump(&self.metrics.sessions_created);
        self.audit.record(
            AuditEvent::new(AuditKind::SessionCreate)
                .session(&name, generation)
                .detail(format!("kill_domain={}", self.kill.tier().label())),
        );
        Ok(SessionCredential {
            session: name,
            generation,
            token,
        })
    }

    fn workspace(&self) -> PathBuf {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/"))
    }

    /// Attach. The token must already have been verified by the attach surface;
    /// the generation is re-checked here as the authoritative fence.
    ///
    /// Lock discipline: the CAS, the eviction decision, and replay registration
    /// all happen under the state lock (buffer lock nested inside it, never the
    /// reverse); every socket write happens after both are dropped.
    pub fn attach(
        &self,
        name: &SessionName,
        generation: Generation,
        size: WindowSize,
        source: Option<&str>,
    ) -> Result<AttachedConnection, Error> {
        let session = match self.get(name) {
            Some(session) => session,
            None => {
                return Err(match self.peek_tombstone(name) {
                    // Distinct from NotFound: the caller offers restart-in-place.
                    Some(_) => Error::SessionDead(name.clone()),
                    None => Error::NotFound(name.clone()),
                })
            }
        };
        if !session.is_alive() {
            return Err(Error::SessionDead(name.clone()));
        }

        let now = Instant::now();
        let evicted: Option<Arc<Outbound>>;
        let conn: ConnGeneration;
        let replay: Option<Replay>;
        let was_takeover: bool;
        {
            let mut state = session.state.lock();
            if state.generation != generation {
                return Err(Error::Unauthorized);
            }
            was_takeover = state.owner_conn != ConnGeneration(0);
            if was_takeover && !state.takeovers.allow(generation, now, &self.policy) {
                Metrics::bump(&self.metrics.takeovers_rate_limited);
                self.audit.record(
                    AuditEvent::new(AuditKind::TakeoverAnomaly)
                        .session(name, generation)
                        .source(source.unwrap_or("unknown"))
                        .detail("takeover_rate_limited"),
                );
                return Err(Error::CapacityExceeded {
                    limit: self.policy.max_takeovers_per_window as usize,
                });
            }
            if !was_takeover {
                // Keep the limiter's generation view current even when the first
                // attach is not a preempt, so a later takeover is charged to the
                // right bucket.
                state.takeovers.allow(generation, now, &self.policy);
            }

            // Attach CAS.
            conn = ConnGeneration(state.next_conn);
            state.next_conn += 1;
            state.owner_conn = conn;
            state.attached_since = Some(now);
            state.missed_pings = 0;
            state.size = size;
            session.io.owner_conn.store(conn.0, Ordering::Release);

            // Replay registration, under the buffer lock nested inside the state
            // lock. Capturing the end offset here is what makes the
            // replay-to-live handoff atomic: live bytes written after this point
            // go to the new subscriber, not into the replay slice.
            let buffer = session.io.buffer.lock();
            replay = if self.config.scrollback_replay && buffer.retention_enabled() {
                Some(buffer.read_since(0))
            } else {
                None
            };
            drop(buffer);

            evicted = session
                .io
                .subscriber
                .lock()
                .replace(Outbound::new(conn, session.backlog_bounds));
        }

        // I/O only after the locks are gone, fenced by `conn`.
        if let Some(previous) = evicted {
            // The replaced connection's write path is already dead — the fence
            // moved before we touched its socket — and closing it can never
            // affect its successor, which owns a different Outbound.
            previous.close(close_code::TAKEOVER);
            Metrics::bump(&self.metrics.takeovers);
            self.audit.record(
                AuditEvent::new(AuditKind::TakeoverAnomaly)
                    .session(name, generation)
                    .source(source.unwrap_or("unknown"))
                    .detail("connection_preempted"),
            );
        }

        let outbound = session
            .io
            .subscriber_for(conn)
            .ok_or_else(|| Error::Other("attach lost the CAS immediately".into()))?;
        outbound.push_control(ControlEvent::AttachNotice {
            ephemeral_workspace: true,
            teardown_best_effort: session.tier_best_effort,
            externalise_with: "git push (lifecycle hooks are backup, not primary)",
        });
        if let Some(replay) = replay {
            match replay {
                Replay::Contiguous { bytes, .. } => outbound.push_bytes(&bytes),
                Replay::Gap {
                    bytes_dropped,
                    bytes,
                    ..
                } => {
                    outbound.push_control(ControlEvent::Gap { bytes_dropped });
                    outbound.push_bytes(&bytes);
                }
            }
        }
        // Attach-time initial size is part of resize propagation, not an extra.
        let _ = session.control.resize(size);
        session.io.touch(self.epoch);
        self.audit.record(
            AuditEvent::new(AuditKind::Attach)
                .session(name, generation)
                .source(source.unwrap_or("unknown")),
        );
        Ok(AttachedConnection {
            session: name.clone(),
            generation,
            conn,
            outbound,
            session_handle: session,
        })
    }

    /// Incremental reconnect within the retained window.
    pub fn replay_since(&self, name: &SessionName, since: u64) -> Result<Replay, Error> {
        let session = self.get(name).ok_or_else(|| Error::NotFound(name.clone()))?;
        // Copy the replay out and drop the guard before returning: a guard in a
        // tail expression outlives the local it borrows from.
        let replay = session.io.buffer.lock().read_since(since);
        Ok(replay)
    }

    /// Remote-admin `renew`: the process survives, the generation bumps, every
    /// outstanding token dies, and an attached connection is **connection-evicted**
    /// with a renew-distinct close code so the client can tell renewal from a
    /// takeover.
    pub fn renew(&self, name: &SessionName) -> Result<SessionCredential, Error> {
        let session = self.get(name).ok_or_else(|| Error::NotFound(name.clone()))?;
        if !session.is_alive() {
            return Err(Error::SessionDead(name.clone()));
        }
        let (generation, evicted) = {
            let mut state = session.state.lock();
            let generation = Generation(state.generation.0 + 1);
            state.generation = generation;
            // Revocation before anything else, per the total order: a token from
            // the old generation must not be able to win an attach in between.
            self.tokens.revoke_session(name, generation);
            state.owner_conn = ConnGeneration(0);
            state.attached_since = None;
            state.missed_pings = 0;
            session.io.owner_conn.store(0, Ordering::Release);
            let evicted = session.io.subscriber.lock().take();
            (generation, evicted)
        };
        if let Some(previous) = evicted {
            previous.close(close_code::RENEWED);
        }
        let token = self.tokens.mint_for_session(name.clone(), generation)?;
        self.audit.record(
            AuditEvent::new(AuditKind::SessionRenew).session(name, generation),
        );
        Ok(SessionCredential {
            session: name.clone(),
            generation,
            token,
        })
    }

    /// Remote-admin `kill`.
    pub async fn kill(&self, name: &SessionName) -> Result<TeardownReport, Error> {
        let session = self.get(name).ok_or_else(|| Error::NotFound(name.clone()))?;
        let report = self
            .teardown(&session, TerminationClass::UserKill, close_code::TTL_EXPIRED)
            .await;
        self.forget(name);
        self.remember_dead(name, session.generation(), TerminationClass::UserKill);
        Ok(report)
    }

    /// Runtime shutdown: every session ends with the runtime-replaced code so
    /// clients can say "your task was replaced, the workspace was reset".
    pub async fn shutdown(&self) -> Vec<TeardownReport> {
        let sessions: Vec<Arc<Session>> = self.state.lock().sessions.values().cloned().collect();
        let mut reports = Vec::new();
        for session in sessions {
            let name = session.name.clone();
            reports.push(
                self.teardown(
                    &session,
                    TerminationClass::RuntimeShutdown,
                    close_code::RUNTIME_REPLACED,
                )
                .await,
            );
            self.forget(&name);
        }
        reports
    }

    /// **session-teardown** — ends the session. Never conflated with
    /// connection-evict, which ends only a connection.
    ///
    /// Order, exactly as the ADR states it: notify client → close socket → kill
    /// per the kill domain → close master fd → release slot. Buffers are cleared
    /// and scrollback never touches disk.
    async fn teardown(
        &self,
        session: &Arc<Session>,
        class: TerminationClass,
        code: u16,
    ) -> TeardownReport {
        let (generation, subscriber, already_dead) = {
            let mut state = session.state.lock();
            let already_dead = !state.alive;
            state.alive = false;
            state.termination = Some(class);
            state.owner_conn = ConnGeneration(0);
            session.io.alive.store(false, Ordering::Release);
            session.io.owner_conn.store(0, Ordering::Release);
            (
                state.generation,
                session.io.subscriber.lock().take(),
                already_dead,
            )
        };
        if !already_dead {
            self.tokens.revoke_session(&session.name, generation);
        }

        // 1+2. Notify then close. Both are non-blocking: teardown must not wait
        // for a client to drain, or a non-reading client could delay expiry.
        if let Some(outbound) = subscriber {
            outbound.close(code);
        }

        // 3. Kill per the active tier. The kill-domain lock is taken only here,
        // never under the state lock.
        let outcome = {
            let mut kill = session.kill.lock().await;
            let outcome = kill.terminate(self.policy.teardown_grace).await;
            // 4+5. Close the master fd and release the slot: dropping the input
            // sender ends the writer thread, and releasing the kill domain frees
            // its tracking budget and cgroup.
            kill.release();
            outcome
        };
        session.io.buffer.lock().clear();

        self.audit.record(
            AuditEvent::new(AuditKind::SessionKill)
                .session(&session.name, generation)
                .termination(class)
                .detail(format!(
                    "{}; survivors={}; best_effort={}",
                    outcome.tier.label(),
                    outcome.survivors.len(),
                    outcome.is_best_effort()
                )),
        );
        TeardownReport {
            session: session.name.as_str().to_owned(),
            generation: generation.0,
            class,
            close_code: code,
            kill_domain: outcome,
        }
    }

    /// The child exited on its own.
    async fn handle_self_exit(self: &Arc<Self>, name: SessionName) {
        let Some(session) = self.get(&name) else {
            return;
        };
        if !session.is_alive() {
            return;
        }
        Metrics::bump(&self.metrics.self_exits);
        // Reap first, so convergence only chases surviving descendants.
        let _ = session.control.try_wait();
        let report = self
            .teardown(
                &session,
                TerminationClass::SelfExit,
                close_code::SESSION_ENDED,
            )
            .await;
        self.forget(&name);
        self.remember_dead(
            &name,
            Generation(report.generation),
            TerminationClass::SelfExit,
        );
    }

    fn forget(&self, name: &SessionName) {
        self.state.lock().sessions.remove(name);
    }

    fn remember_dead(&self, name: &SessionName, generation: Generation, class: TerminationClass) {
        let mut state = self.state.lock();
        state
            .tombstones
            .push_back((name.clone(), Tombstone { generation, class }));
        while state.tombstones.len() > MAX_TOMBSTONES {
            state.tombstones.pop_front();
        }
    }

    fn peek_tombstone(&self, name: &SessionName) -> Option<TerminationClass> {
        self.state
            .lock()
            .tombstones
            .iter()
            .rev()
            .find(|(dead, _)| dead == name)
            .map(|(_, tombstone)| tombstone.class)
    }

    fn take_tombstone(&self, name: &SessionName) -> Option<Tombstone> {
        let mut state = self.state.lock();
        let index = state
            .tombstones
            .iter()
            .rposition(|(dead, _)| dead == name)?;
        state
            .tombstones
            .remove(index)
            .map(|(_, tombstone)| tombstone)
    }

    /// Record a session's workspace usage. Returns the first-class condition
    /// when the per-session budget is exhausted, and tells the attached client
    /// through its own control frame rather than an opaque write error.
    pub fn report_session_disk_usage(&self, name: &SessionName, used_kib: u64) -> bool {
        let Some(session) = self.get(name) else {
            return false;
        };
        let (full, subscriber) = {
            let mut state = session.state.lock();
            state.disk_used_kib = used_kib;
            let full = used_kib >= self.config.per_session_disk_kib as u64;
            (full, session.io.subscriber.lock().clone())
        };
        if full {
            Metrics::bump(&self.metrics.workspace_full);
            if let Some(outbound) = subscriber {
                outbound.push_control(ControlEvent::WorkspaceVolumeFull {
                    used_kib,
                    budget_kib: self.config.per_session_disk_kib as u64,
                });
            }
        }
        full
    }

    /// TTL maintenance. Call on a timer; returns what it did so the behaviour is
    /// assertable without a wall clock.
    ///
    /// Expiry is client-visible: a warning control frame precedes forced
    /// teardown, and teardown closes with `TTL_EXPIRED`.
    pub async fn tick(self: &Arc<Self>) -> Vec<TtlAction> {
        let now = Instant::now();
        let sessions: Vec<Arc<Session>> = self.state.lock().sessions.values().cloned().collect();
        let mut actions = Vec::new();
        for session in sessions {
            // Copy the decision out under the lock; act after dropping it.
            let decision = {
                let mut state = session.state.lock();
                if !state.alive {
                    continue;
                }
                let absolute_deadline = state.created_at + self.policy.absolute_ttl;
                let idle_deadline = if state.owner_conn == ConnGeneration(0) {
                    Some(self.last_activity(&session) + self.policy.detached_idle_ttl)
                } else {
                    None
                };
                if now >= absolute_deadline {
                    Some(TtlDecision::Expire { absolute: true })
                } else if idle_deadline.is_some_and(|deadline| now >= deadline) {
                    Some(TtlDecision::Expire { absolute: false })
                } else if !state.warned_absolute
                    && now + self.policy.ttl_warning_lead >= absolute_deadline
                {
                    state.warned_absolute = true;
                    Some(TtlDecision::Warn {
                        absolute: true,
                        seconds_remaining: absolute_deadline.duration_since(now).as_secs(),
                    })
                } else if !state.warned_idle
                    && idle_deadline
                        .is_some_and(|deadline| now + self.policy.ttl_warning_lead >= deadline)
                {
                    state.warned_idle = true;
                    idle_deadline.map(|deadline| TtlDecision::Warn {
                        absolute: false,
                        seconds_remaining: deadline.duration_since(now).as_secs(),
                    })
                } else {
                    None
                }
            };
            let Some(decision) = decision else { continue };
            match decision {
                TtlDecision::Warn {
                    absolute,
                    seconds_remaining,
                } => {
                    let subscriber = session.io.subscriber.lock().clone();
                    if let Some(outbound) = subscriber {
                        outbound.push_control(ControlEvent::TtlWarning {
                            seconds_remaining,
                            absolute,
                        });
                    }
                    actions.push(TtlAction::Warned {
                        session: session.name.clone(),
                        absolute,
                    });
                }
                TtlDecision::Expire { absolute } => {
                    let name = session.name.clone();
                    self.teardown(
                        &session,
                        TerminationClass::UserKill,
                        close_code::TTL_EXPIRED,
                    )
                    .await;
                    self.forget(&name);
                    self.remember_dead(&name, session.generation(), TerminationClass::UserKill);
                    Metrics::bump(&self.metrics.ttl_expired);
                    actions.push(TtlAction::Expired {
                        session: name,
                        absolute,
                    });
                }
            }
        }
        actions
    }

    fn last_activity(&self, session: &Arc<Session>) -> Instant {
        self.epoch
            + Duration::from_millis(session.io.last_activity_ms.load(Ordering::Relaxed))
    }

    /// Called by the reader pump for each PTY chunk: retain it, then hand it to
    /// the current subscriber.
    fn on_output(&self, session: &Arc<Session>, bytes: &[u8]) {
        // Buffer lock only. The state lock is never on the output path, so a
        // busy session cannot slow an attach, a renew, or a teardown.
        session.io.buffer.lock().write(bytes);
        session.io.touch(self.epoch);
        let subscriber = session.io.subscriber.lock().clone();
        if let Some(outbound) = subscriber {
            outbound.push_bytes(bytes);
        }
    }
}

enum TtlDecision {
    Warn { absolute: bool, seconds_remaining: u64 },
    Expire { absolute: bool },
}

/// What a [`SessionManager::tick`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TtlAction {
    Warned { session: SessionName, absolute: bool },
    Expired { session: SessionName, absolute: bool },
}

/// Writer thread: client bytes → PTY master.
///
/// A dedicated OS thread, not a blocking-pool task, because a `write` to a PTY
/// master can block indefinitely on a stopped reader and must not occupy a
/// runtime worker. It honours only the current connection generation, so bytes
/// from a replaced connection are discarded even if they were already in flight.
fn spawn_writer_thread(
    io: Arc<SessionIo>,
    mut input: tokio::sync::mpsc::Receiver<InputChunk>,
    mut writer: Box<dyn Write + Send>,
) {
    std::thread::spawn(move || {
        while let Some(chunk) = input.blocking_recv() {
            if io.owner_conn.load(Ordering::Acquire) != chunk.conn.0 {
                continue;
            }
            if writer.write_all(&chunk.bytes).is_err() {
                break;
            }
            let _ = writer.flush();
        }
    });
}

/// Reader thread: PTY master → ring buffer and subscriber.
///
/// EOF is the child's self-exit signal: teardown closes the master, so the read
/// returns and this thread ends.
fn spawn_reader_thread(
    manager: Arc<SessionManager>,
    session: Arc<Session>,
    mut reader: Box<dyn Read + Send>,
) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    tokio::spawn(async move {
        while let Some(chunk) = rx.recv().await {
            manager.on_output(&session, &chunk);
        }
        // The stream ended. If the session is still marked alive this is a
        // self-exit, which is a distinct termination class from a kill.
        if session.is_alive() {
            manager.handle_self_exit(session.name.clone()).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::containment::SecretBytes;
    use crate::killdomain::{
        KillDomainSelection, ProcessSignals, Signal, Tier2Unavailable, TrackingLimits,
    };
    use std::io;
    use std::sync::mpsc as std_mpsc;

    const HASH: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn config(overrides: &str) -> PtyConfig {
        let text = format!(
            "[pty]\nenabled = true\ncommand = \"/bin/sh\"\nadmin_credential_hash = \"{HASH}\"\n{overrides}"
        );
        crate::config::validate_projection(&text).expect("test projection must validate")
    }

    /// Nothing here spawns a process: the fake PTY is a pair of in-memory pipes
    /// plus a resize/exit record.
    #[derive(Debug, Default)]
    struct FakePtyState {
        resizes: Mutex<Vec<WindowSize>>,
        exit: Mutex<Option<i32>>,
    }

    struct FakeControl(Arc<FakePtyState>);

    impl PtyControl for FakeControl {
        fn resize(&self, size: WindowSize) -> Result<(), Error> {
            self.0.resizes.lock().push(size);
            Ok(())
        }
        fn try_wait(&self) -> Result<Option<i32>, Error> {
            Ok(*self.0.exit.lock())
        }
    }

    /// Reader end fed by the test: `output.send(...)` makes the session see PTY
    /// bytes; dropping the sender is EOF, i.e. child self-exit.
    struct ChannelReader(std_mpsc::Receiver<Vec<u8>>, Vec<u8>);

    impl Read for ChannelReader {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.1.is_empty() {
                match self.0.recv() {
                    Ok(bytes) => self.1 = bytes,
                    Err(_) => return Ok(0),
                }
            }
            let n = out.len().min(self.1.len());
            out[..n].copy_from_slice(&self.1[..n]);
            self.1.drain(..n);
            Ok(n)
        }
    }

    struct RecordingWriter(Arc<Mutex<Vec<u8>>>, Arc<AtomicBool>);

    impl Write for RecordingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            // A stalled PTY master: the writer thread parks here, so the bounded
            // input queue is the only thing standing between a flooding client
            // and unbounded memory.
            while self.1.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.0.lock().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FakeSpawner {
        pty: Arc<FakePtyState>,
        written: Arc<Mutex<Vec<u8>>>,
        output: Mutex<Option<std_mpsc::Sender<Vec<u8>>>>,
        last_env: Mutex<Vec<(String, String)>>,
        stall: Arc<AtomicBool>,
        fail: bool,
    }

    impl FakeSpawner {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                pty: Arc::new(FakePtyState::default()),
                written: Arc::new(Mutex::new(Vec::new())),
                output: Mutex::new(None),
                last_env: Mutex::new(Vec::new()),
                stall: Arc::new(AtomicBool::new(false)),
                fail: false,
            })
        }

        fn failing() -> Arc<Self> {
            Arc::new(Self {
                pty: Arc::new(FakePtyState::default()),
                written: Arc::new(Mutex::new(Vec::new())),
                output: Mutex::new(None),
                last_env: Mutex::new(Vec::new()),
                stall: Arc::new(AtomicBool::new(false)),
                fail: true,
            })
        }

        fn emit(&self, bytes: &[u8]) {
            let sender = self.output.lock().clone();
            if let Some(sender) = sender {
                sender.send(bytes.to_vec()).unwrap();
            }
        }

        /// Simulate the child exiting on its own: EOF on the master.
        fn end_child(&self, code: i32) {
            *self.pty.exit.lock() = Some(code);
            *self.output.lock() = None;
        }
    }

    impl PtySpawner for FakeSpawner {
        fn spawn(
            &self,
            request: SpawnRequest,
            containment: &mut SessionKillDomain,
        ) -> Result<SpawnedPty, Error> {
            if self.fail {
                return Err(Error::Other("synthetic spawn failure".into()));
            }
            *self.last_env.lock() = request.env.clone();
            let (tx, rx) = std_mpsc::channel();
            *self.output.lock() = Some(tx);
            self.pty.resizes.lock().push(request.size);
            containment.set_leader(4242)?;
            Ok(SpawnedPty {
                pid: Some(4242),
                reader: Box::new(ChannelReader(rx, Vec::new())),
                writer: Box::new(RecordingWriter(self.written.clone(), self.stall.clone())),
                control: Arc::new(FakeControl(self.pty.clone())),
            })
        }

        fn capability(&self) -> crate::killdomain::SpawnCapability {
            crate::killdomain::SpawnCapability::ProcessGroupOnly
        }
    }

    /// Every fake pid is already gone, so teardown converges instantly.
    #[derive(Debug)]
    struct DeadSignals;

    impl ProcessSignals for DeadSignals {
        fn signal_group(&self, _pgid: i32, _signal: Signal) -> io::Result<()> {
            Ok(())
        }
        fn signal_pid(&self, _pid: i32, _signal: Signal) -> io::Result<()> {
            Ok(())
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

    fn kill_domain() -> Arc<KillDomain> {
        Arc::new(KillDomain::with_signals(
            KillDomainSelection::tier1(Tier2Unavailable::NotLinux),
            TrackingLimits::default(),
            AuditLogger,
            Arc::new(DeadSignals),
        ))
    }

    fn manager_with(
        spawner: Arc<dyn PtySpawner>,
        overrides: &str,
        policy: SessionPolicy,
    ) -> Arc<SessionManager> {
        SessionManager::new(
            config(overrides),
            policy,
            TokenStore::with_default_ttl(AuditLogger),
            AuditLogger,
            kill_domain(),
            spawner,
        )
        .unwrap()
    }

    fn name(text: &str) -> SessionName {
        SessionName::parse(text).unwrap()
    }

    fn drain(outbound: &Arc<Outbound>) -> Vec<OutboundItem> {
        let mut items = Vec::new();
        while let Some(item) = outbound.try_next() {
            let done = matches!(item, OutboundItem::Close(_));
            items.push(item);
            if done {
                break;
            }
        }
        items
    }

    fn collected_bytes(items: &[OutboundItem]) -> Vec<u8> {
        items
            .iter()
            .filter_map(|item| match item {
                OutboundItem::Bytes(bytes) => Some(bytes.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    // ---- environment ----------------------------------------------------

    #[test]
    fn child_env_is_an_allowlist_and_drops_every_credential_shaped_variable() {
        let source = [
            ("TERM", "screen"),
            ("PATH", "/usr/bin"),
            ("HOME", "/workspace"),
            ("USER", "pty"),
            ("SHELL", "/bin/sh"),
            ("LANG", "en_US.UTF-8"),
            ("LC_ALL", "en_US.UTF-8"),
            ("OPENAB_ACP_AUTH_KEY", "leak"),
            ("DISCORD_BOT_TOKEN", "leak"),
            ("AWS_SECRET_ACCESS_KEY", "leak"),
            ("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI", "leak"),
            ("OPENAB_PTY_ADMIN", "leak"),
            ("SSH_AUTH_SOCK", "leak"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));

        let env = child_env(source, "/workspace", None);
        let keys: Vec<&str> = env.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["HOME", "LANG", "LC_ALL", "PATH", "SHELL", "TERM", "USER"]
        );
        assert!(
            !env.iter().any(|(_, value)| value == "leak"),
            "no credential-shaped variable may reach the child: {env:?}"
        );
    }

    #[test]
    fn child_env_supplies_term_and_home_when_absent() {
        let env = child_env([("PATH".to_string(), "/bin".to_string())], "/workspace", None);
        assert!(env.contains(&("TERM".to_string(), DEFAULT_TERM.to_string())));
        assert!(env.contains(&("HOME".to_string(), "/workspace".to_string())));
    }

    // ---- admission ------------------------------------------------------

    #[tokio::test]
    async fn create_enforces_max_sessions_and_rejects_duplicates() {
        let manager = manager_with(FakeSpawner::new(), "max_sessions = 1\n", SessionPolicy::default());
        manager.create(name("one"), WindowSize::default()).unwrap();
        assert!(matches!(
            manager.create(name("one"), WindowSize::default()),
            Err(Error::AlreadyExists(_))
        ));
        assert!(matches!(
            manager.create(name("two"), WindowSize::default()),
            Err(Error::CapacityExceeded { limit: 1 })
        ));
        assert_eq!(manager.metrics().admission_rejected.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_full_workspace_volume_is_a_first_class_admission_failure() {
        let manager = manager_with(FakeSpawner::new(), "", SessionPolicy::default());
        manager.report_volume_usage(manager.config.volume_capacity_kib as u64);
        let Err(error) = manager.create(name("blocked"), WindowSize::default()) else {
            panic!("must refuse rather than fail opaquely inside the terminal");
        };
        assert!(error.to_string().contains("workspace volume full"));
        assert_eq!(manager.metrics().workspace_full.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_failed_spawn_leaves_no_session_and_no_tracking_budget_held() {
        let manager = manager_with(FakeSpawner::failing(), "", SessionPolicy::default());
        assert!(manager.create(name("nope"), WindowSize::default()).is_err());
        assert!(manager.list().is_empty());
        assert_eq!(manager.kill_domain().status().tracked_processes, 0);
    }

    #[tokio::test]
    async fn session_status_labels_the_best_effort_tier() {
        let manager = manager_with(FakeSpawner::new(), "", SessionPolicy::default());
        manager.create(name("s"), WindowSize::default()).unwrap();
        let status = &manager.list()[0];
        assert!(status.teardown_best_effort);
        assert!(status.absolute_ttl_best_effort);
        assert_eq!(status.tier, KillDomainTier::Tier1BestEffortProcessGroup);
    }

    // ---- attach / takeover ----------------------------------------------

    #[tokio::test]
    async fn attach_requires_the_current_generation() {
        let manager = manager_with(FakeSpawner::new(), "", SessionPolicy::default());
        let created = manager.create(name("gen"), WindowSize::default()).unwrap();
        assert!(manager
            .attach(&name("gen"), Generation(created.generation.0 + 1), WindowSize::default(), None)
            .is_err());
        assert!(manager
            .attach(&name("gen"), created.generation, WindowSize::default(), None)
            .is_ok());
    }

    #[tokio::test]
    async fn takeover_evicts_the_incumbent_and_the_successor_is_unaffected() {
        let manager = manager_with(FakeSpawner::new(), "", SessionPolicy::default());
        let created = manager.create(name("cas"), WindowSize::default()).unwrap();
        let first = manager
            .attach(&name("cas"), created.generation, WindowSize::default(), Some("a"))
            .unwrap();
        let second = manager
            .attach(&name("cas"), created.generation, WindowSize::default(), Some("b"))
            .unwrap();

        // The replaced connection's write path is dead before its socket closes.
        assert_eq!(first.write_input(b"x"), Err(InputError::Evicted));
        assert_eq!(first.outbound.close_code(), Some(close_code::TAKEOVER));

        // Tearing down the replaced connection cannot affect its successor.
        first.detach();
        drop(first);
        assert!(second.outbound.close_code().is_none());
        assert!(second.write_input(b"y").is_ok());
        assert_eq!(manager.metrics().takeovers.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn takeovers_are_rate_limited_per_session() {
        let policy = SessionPolicy {
            max_takeovers_per_window: 2,
            ..SessionPolicy::default()
        };
        let manager = manager_with(FakeSpawner::new(), "", policy);
        let created = manager.create(name("abuse"), WindowSize::default()).unwrap();
        let session = name("abuse");
        // First attach is not a preempt; then two allowed takeovers.
        manager.attach(&session, created.generation, WindowSize::default(), None).unwrap();
        manager.attach(&session, created.generation, WindowSize::default(), None).unwrap();
        manager.attach(&session, created.generation, WindowSize::default(), None).unwrap();
        assert!(matches!(
            manager.attach(&session, created.generation, WindowSize::default(), None),
            Err(Error::CapacityExceeded { .. })
        ));
        assert_eq!(
            manager.metrics().takeovers_rate_limited.load(Ordering::Relaxed),
            1
        );
    }

    #[tokio::test]
    async fn a_generation_bump_always_reopens_the_recovery_path() {
        // A thief who exhausted the takeover budget must not be able to lock the
        // victim out of `session renew`.
        let policy = SessionPolicy {
            max_takeovers_per_window: 1,
            ..SessionPolicy::default()
        };
        let manager = manager_with(FakeSpawner::new(), "", policy);
        let created = manager.create(name("recover"), WindowSize::default()).unwrap();
        let session = name("recover");
        manager.attach(&session, created.generation, WindowSize::default(), None).unwrap();
        manager.attach(&session, created.generation, WindowSize::default(), None).unwrap();
        assert!(manager
            .attach(&session, created.generation, WindowSize::default(), None)
            .is_err());

        let renewed = manager.renew(&session).unwrap();
        assert_eq!(renewed.generation, Generation(created.generation.0 + 1));
        assert!(
            manager
                .attach(&session, renewed.generation, WindowSize::default(), None)
                .is_ok(),
            "the first attach of a new generation bypasses an exhausted bucket"
        );
    }

    #[tokio::test]
    async fn renew_evicts_with_a_renew_distinct_code_and_invalidates_old_tokens() {
        let manager = manager_with(FakeSpawner::new(), "", SessionPolicy::default());
        let created = manager.create(name("renew"), WindowSize::default()).unwrap();
        let old_token = created.token.plaintext.as_bytes().to_vec();
        let attached = manager
            .attach(&name("renew"), created.generation, WindowSize::default(), None)
            .unwrap();

        let renewed = manager.renew(&name("renew")).unwrap();
        assert_eq!(
            attached.outbound.close_code(),
            Some(close_code::RENEWED),
            "an evicted client must be able to tell renewal from takeover"
        );
        assert_eq!(attached.write_input(b"x"), Err(InputError::Evicted));

        // The old token no longer verifies, at either generation.
        let verifier = manager.tokens.attach_verifier();
        let mut presented = SecretBytes::new(old_token.clone());
        assert!(verifier
            .verify(&name("renew"), created.generation, &mut presented, None)
            .is_err());
        let mut presented = SecretBytes::new(old_token);
        assert!(verifier
            .verify(&name("renew"), renewed.generation, &mut presented, None)
            .is_err());
        // The process survived: renew is connection-evict, never session-teardown.
        assert!(manager.get(&name("renew")).unwrap().is_alive());
    }

    // ---- I/O, replay, resize -------------------------------------------

    #[tokio::test]
    async fn pty_output_is_retained_and_delivered_to_the_current_connection() {
        let spawner = FakeSpawner::new();
        let manager = manager_with(spawner.clone(), "", SessionPolicy::default());
        let created = manager.create(name("io"), WindowSize::default()).unwrap();
        let attached = manager
            .attach(&name("io"), created.generation, WindowSize::default(), None)
            .unwrap();

        spawner.emit(b"hello ");
        spawner.emit(b"world");
        tokio::time::sleep(Duration::from_millis(50)).await;

        let items = drain(&attached.outbound);
        assert_eq!(collected_bytes(&items), b"hello world");
        assert!(
            matches!(items[0], OutboundItem::Control(ControlEvent::AttachNotice { .. })),
            "attach must state that the workspace is ephemeral: {items:?}"
        );
        assert_eq!(manager.get(&name("io")).unwrap().total_written(), 11);
    }

    #[tokio::test]
    async fn client_input_reaches_the_pty_with_capability_responses_stripped() {
        let spawner = FakeSpawner::new();
        let manager = manager_with(spawner.clone(), "", SessionPolicy::default());
        let created = manager.create(name("in"), WindowSize::default()).unwrap();
        let attached = manager
            .attach(&name("in"), created.generation, WindowSize::default(), None)
            .unwrap();

        attached.write_input(b"ls\x1b[?62;22c -la\r").unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(spawner.written.lock().as_slice(), b"ls -la\r");
    }

    #[tokio::test]
    async fn input_beyond_the_watermark_fails_closed() {
        let spawner = FakeSpawner::new();
        let manager = manager_with(spawner.clone(), "", SessionPolicy::default());
        let created = manager.create(name("flood"), WindowSize::default()).unwrap();
        let attached = manager
            .attach(&name("flood"), created.generation, WindowSize::default(), None)
            .unwrap();

        // Stall the PTY master so nothing drains, then flood: the queue must
        // refuse rather than grow, and the server disconnects such a client.
        spawner.stall.store(true, Ordering::Release);
        let mut rejected = None;
        for _ in 0..(INPUT_QUEUE_CHUNKS * 4) {
            if let Err(error) = attached.write_input(b"x") {
                rejected = Some(error);
                break;
            }
        }
        spawner.stall.store(false, Ordering::Release);
        assert_eq!(
            rejected,
            Some(InputError::Backpressure),
            "input must be bounded, never queued without limit"
        );
    }

    #[tokio::test]
    async fn resize_propagates_and_the_attach_time_size_counts() {
        let spawner = FakeSpawner::new();
        let manager = manager_with(spawner.clone(), "", SessionPolicy::default());
        let created = manager.create(name("size"), WindowSize::default()).unwrap();
        let attached = manager
            .attach(
                &name("size"),
                created.generation,
                WindowSize::validated(40, 120).unwrap(),
                None,
            )
            .unwrap();
        attached.resize(WindowSize::validated(50, 200).unwrap()).unwrap();

        let sizes = spawner.pty.resizes.lock().clone();
        assert_eq!(sizes[0], WindowSize::default(), "spawn size");
        assert_eq!(sizes[1], WindowSize { rows: 40, cols: 120 }, "attach-time size");
        assert_eq!(sizes[2], WindowSize { rows: 50, cols: 200 }, "resize frame");
        assert_eq!(
            manager.get(&name("size")).unwrap().window_size(),
            WindowSize { rows: 50, cols: 200 }
        );
        assert!(WindowSize::validated(0, 80).is_err());
        assert!(WindowSize::validated(24, 5000).is_err());
    }

    #[tokio::test]
    async fn a_since_cursor_replays_only_missed_bytes() {
        let spawner = FakeSpawner::new();
        let manager = manager_with(spawner.clone(), "", SessionPolicy::default());
        manager.create(name("cursor"), WindowSize::default()).unwrap();
        spawner.emit(b"abcdef");
        tokio::time::sleep(Duration::from_millis(50)).await;
        match manager.replay_since(&name("cursor"), 3).unwrap() {
            Replay::Contiguous { bytes, to_offset } => {
                assert_eq!(bytes, b"def");
                assert_eq!(to_offset, 6);
            }
            other => panic!("expected contiguous replay, got {other:?}"),
        }
    }

    // ---- termination ----------------------------------------------------

    #[tokio::test]
    async fn kill_is_a_session_teardown_that_clears_state_and_tokens() {
        let manager = manager_with(FakeSpawner::new(), "", SessionPolicy::default());
        let created = manager.create(name("bye"), WindowSize::default()).unwrap();
        let attached = manager
            .attach(&name("bye"), created.generation, WindowSize::default(), None)
            .unwrap();

        let report = manager.kill(&name("bye")).await.unwrap();
        assert_eq!(report.class, TerminationClass::UserKill);
        assert!(report.kill_domain.converged());
        assert!(report.kill_domain.is_best_effort());
        assert!(attached.outbound.close_code().is_some());
        assert!(manager.list().is_empty(), "the slot is released");

        // Reattach-to-dead is a distinct error offering restart-in-place.
        assert!(matches!(
            manager.attach(&name("bye"), created.generation, WindowSize::default(), None),
            Err(Error::SessionDead(_))
        ));
        assert!(matches!(
            manager.attach(&name("never-existed"), Generation(1), WindowSize::default(), None),
            Err(Error::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn teardown_is_not_blocked_by_a_client_that_never_reads() {
        let manager = manager_with(FakeSpawner::new(), "", SessionPolicy::default());
        let created = manager.create(name("slow"), WindowSize::default()).unwrap();
        let attached = manager
            .attach(&name("slow"), created.generation, WindowSize::default(), None)
            .unwrap();
        // Nothing is ever drained from `attached.outbound`.
        let killed = tokio::time::timeout(Duration::from_secs(2), manager.kill(&name("slow"))).await;
        assert!(killed.is_ok(), "a non-reading client must not delay teardown");
        assert!(attached.outbound.close_code().is_some());
    }

    #[tokio::test]
    async fn self_exit_ends_with_session_ended_and_offers_restart_in_place() {
        let spawner = FakeSpawner::new();
        let manager = manager_with(spawner.clone(), "", SessionPolicy::default());
        let created = manager.create(name("exit"), WindowSize::default()).unwrap();
        let attached = manager
            .attach(&name("exit"), created.generation, WindowSize::default(), None)
            .unwrap();
        spawner.emit(b"final output");
        spawner.end_child(0);

        // The reader thread sees EOF; wait for the manager to converge.
        for _ in 0..100 {
            if manager.get(&name("exit")).is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(manager.get(&name("exit")).is_none(), "slot released");
        let items = drain(&attached.outbound);
        assert!(
            collected_bytes(&items).ends_with(b"final output"),
            "the final flush must reach the client: {items:?}"
        );
        assert_eq!(
            attached.outbound.close_code(),
            Some(close_code::SESSION_ENDED)
        );
        assert_eq!(manager.metrics().self_exits.load(Ordering::Relaxed), 1);

        let restarted = manager.restart_in_place(&name("exit")).await.unwrap();
        assert!(
            restarted.generation > created.generation,
            "restart-in-place mints a new generation"
        );
        let session = manager.get(&name("exit")).unwrap();
        assert!(session.is_alive());
        assert_eq!(session.total_written(), 0, "empty scrollback");
    }

    #[tokio::test]
    async fn shutdown_uses_the_runtime_replaced_close_code() {
        let manager = manager_with(FakeSpawner::new(), "", SessionPolicy::default());
        let created = manager.create(name("shut"), WindowSize::default()).unwrap();
        let attached = manager
            .attach(&name("shut"), created.generation, WindowSize::default(), None)
            .unwrap();
        let reports = manager.shutdown().await;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].class, TerminationClass::RuntimeShutdown);
        assert_eq!(
            attached.outbound.close_code(),
            Some(close_code::RUNTIME_REPLACED)
        );
    }

    // ---- TTLs -----------------------------------------------------------

    // TTL tests use short real durations rather than tokio's paused clock: the
    // crate's dev-dependencies do not enable tokio's `test-util` feature, and
    // adding it is outside this change.
    #[tokio::test]
    async fn the_absolute_ttl_applies_even_while_attached() {
        let policy = SessionPolicy {
            absolute_ttl: Duration::from_millis(400),
            ttl_warning_lead: Duration::from_millis(250),
            ..SessionPolicy::default()
        };
        let manager = manager_with(FakeSpawner::new(), "", policy);
        let created = manager.create(name("cap"), WindowSize::default()).unwrap();
        let attached = manager
            .attach(&name("cap"), created.generation, WindowSize::default(), None)
            .unwrap();
        drain(&attached.outbound);

        assert!(manager.tick().await.is_empty(), "nothing is due yet");

        tokio::time::sleep(Duration::from_millis(200)).await;
        let warned = manager.tick().await;
        assert_eq!(
            warned,
            vec![TtlAction::Warned {
                session: name("cap"),
                absolute: true
            }]
        );
        let items = drain(&attached.outbound);
        assert!(
            items.iter().any(|item| matches!(
                item,
                OutboundItem::Control(ControlEvent::TtlWarning { absolute: true, .. })
            )),
            "expiry must be client-visible before teardown: {items:?}"
        );

        tokio::time::sleep(Duration::from_millis(250)).await;
        let expired = manager.tick().await;
        assert_eq!(
            expired,
            vec![TtlAction::Expired {
                session: name("cap"),
                absolute: true
            }]
        );
        assert_eq!(attached.outbound.close_code(), Some(close_code::TTL_EXPIRED));
        assert!(manager.list().is_empty());
    }

    #[tokio::test]
    async fn the_detached_idle_ttl_only_runs_while_detached() {
        let policy = SessionPolicy {
            detached_idle_ttl: Duration::from_millis(200),
            ttl_warning_lead: Duration::from_millis(1),
            ..SessionPolicy::default()
        };
        let manager = manager_with(FakeSpawner::new(), "", policy);
        let created = manager.create(name("idle"), WindowSize::default()).unwrap();
        let attached = manager
            .attach(&name("idle"), created.generation, WindowSize::default(), None)
            .unwrap();

        // Attached: a live socket is liveness, however idle the user is.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(manager.tick().await.is_empty());

        attached.detach();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let actions = manager.tick().await;
        assert_eq!(
            actions,
            vec![TtlAction::Expired {
                session: name("idle"),
                absolute: false
            }]
        );
    }

    #[tokio::test]
    async fn missed_pings_count_as_detached_and_evict_the_connection() {
        let manager = manager_with(FakeSpawner::new(), "", SessionPolicy::default());
        let created = manager.create(name("ping"), WindowSize::default()).unwrap();
        let attached = manager
            .attach(&name("ping"), created.generation, WindowSize::default(), None)
            .unwrap();
        assert!(!attached.on_missed_ping());
        attached.on_pong();
        assert!(!attached.on_missed_ping());
        assert!(!attached.on_missed_ping());
        assert!(
            attached.on_missed_ping(),
            "3 missed pings is the configured detached threshold"
        );
        let session = manager.get(&name("ping")).unwrap();
        assert!(!session.is_attached());
        assert!(session.is_alive(), "a half-open socket never kills a session");
    }

    #[test]
    fn policy_bands_from_the_adr_are_enforced() {
        let bad_interval = SessionPolicy {
            ping_interval: Duration::from_secs(60),
            ..SessionPolicy::default()
        };
        assert!(bad_interval.validate().is_err());
        let bad_pings = SessionPolicy {
            missed_pings_before_detached: 9,
            ..SessionPolicy::default()
        };
        assert!(bad_pings.validate().is_err());
        assert!(SessionPolicy::default().validate().is_ok());
    }

    #[tokio::test]
    async fn per_session_disk_budget_surfaces_workspace_full_to_the_client() {
        let manager = manager_with(FakeSpawner::new(), "", SessionPolicy::default());
        let created = manager.create(name("disk"), WindowSize::default()).unwrap();
        let attached = manager
            .attach(&name("disk"), created.generation, WindowSize::default(), None)
            .unwrap();
        assert!(!manager.report_session_disk_usage(&name("disk"), 1));
        assert!(manager.report_session_disk_usage(
            &name("disk"),
            manager.config.per_session_disk_kib as u64
        ));
        let items = drain(&attached.outbound);
        assert!(
            items.iter().any(|item| matches!(
                item,
                OutboundItem::Control(ControlEvent::WorkspaceVolumeFull { .. })
            )),
            "must be a first-class condition, not an opaque write error: {items:?}"
        );
    }

    #[tokio::test]
    async fn a_slow_client_backlog_is_bounded_and_ends_in_slow_client() {
        let manager = manager_with(
            FakeSpawner::new(),
            "outbound_backlog_kib = 1\nscrollback_kib = 1\nfixed_session_overhead_kib = 1\nreplay_queue_kib = 1\n",
            SessionPolicy::default(),
        );
        let created = manager.create(name("backlog"), WindowSize::default()).unwrap();
        let attached = manager
            .attach(&name("backlog"), created.generation, WindowSize::default(), None)
            .unwrap();
        let session = manager.get(&name("backlog")).unwrap();
        let chunk = vec![b'z'; 4096];
        for _ in 0..8 {
            manager.on_output(&session, &chunk);
        }
        assert_eq!(
            attached.outbound.close_code(),
            Some(close_code::SLOW_CLIENT),
            "past the hard watermark the connection is dropped, never queued"
        );
        assert!(attached.outbound.queue.lock().len() <= 1024);
    }

    #[tokio::test]
    async fn outbound_next_terminates_once_closed_and_drained() {
        let manager = manager_with(FakeSpawner::new(), "", SessionPolicy::default());
        let created = manager.create(name("drain"), WindowSize::default()).unwrap();
        let attached = manager
            .attach(&name("drain"), created.generation, WindowSize::default(), None)
            .unwrap();
        attached.outbound.push_bytes(b"tail");
        attached.outbound.close(close_code::TTL_EXPIRED);

        let mut saw_close = false;
        while let Some(item) =
            tokio::time::timeout(Duration::from_secs(1), attached.outbound.next())
                .await
                .expect("next must not hang")
        {
            if let OutboundItem::Close(code) = item {
                assert_eq!(code, close_code::TTL_EXPIRED);
                saw_close = true;
                break;
            }
        }
        assert!(saw_close);
    }

    // ---- integration: a real PTY child --------------------------------------
    // Ignored by default: this is the only test here that leaves the process.

    #[tokio::test]
    #[ignore = "spawns a real shell through portable-pty"]
    async fn the_real_spawner_runs_a_shell_and_reports_self_exit() {
        let manager = manager_with(
            Arc::new(PortablePtySpawner),
            "command = \"/bin/sh\"\n",
            SessionPolicy::default(),
        );
        let created = manager.create(name("real"), WindowSize::default()).unwrap();
        let attached = manager
            .attach(&name("real"), created.generation, WindowSize::default(), None)
            .unwrap();
        attached.write_input(b"exit 0\n").unwrap();

        for _ in 0..200 {
            if manager.get(&name("real")).is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(manager.get(&name("real")).is_none(), "self-exit released the slot");
        assert_eq!(
            attached.outbound.close_code(),
            Some(close_code::SESSION_ENDED)
        );
    }
}

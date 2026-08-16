//! Linux adversary tests for the two security claims the ADR rests on.
//!
//! A throwaway C spike already proved the *kernel* behaves as the ADR assumes.
//! These tests prove the *product code* uses those mechanisms correctly, so a
//! test that passes for the wrong reason is worse than no test at all. Two
//! methodological rules follow from that and are applied throughout:
//!
//! 1. **Every denial assertion has a positive control.** `/proc/sys/kernel/yama/
//!    ptrace_scope` is `1` on the CI host, which means an *unrelated* same-UID
//!    probe is refused by Yama whatever the dumpability is. A naive test would
//!    therefore pass without the barrier doing anything. Each probe here is run
//!    twice against the same parent/child relationship — once with the barrier,
//!    once without — and the barrier-less arm must **succeed**. A pass then
//!    cannot be a Yama artifact, because Yama's answer is identical in both arms.
//! 2. **Only synthetic secrets.** The credential material probed for is
//!    [`FAKE_SECRET`]; nothing here reads a real credential from the host.
//!
//! The whole file is Linux-only (`PR_SET_DUMPABLE` does not exist elsewhere,
//! and the ADR scopes the containment claim to Linux) and every
//! test is `#[ignore]`d, because they spawn real processes, bind real sockets,
//! and read `/proc`. Run them with:
//!
//! ```text
//! cargo test -p openab-pty --test adversary_linux -- --include-ignored
//! ```
#![cfg(target_os = "linux")]

use openab_pty::admin_auth::AdminAuthenticator;
use openab_pty::audit::AuditLogger;
use openab_pty::config;
use openab_pty::containment::{self, ContainmentStatus};
use openab_pty::killdomain::{KillDomain, TrackingLimits};
use openab_pty::session::{PortablePtySpawner, SessionManager, SessionPolicy, WindowSize};
use openab_pty::token::TokenStore;
use openab_pty::SessionName;
use std::io::{Read, Write};
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The only "credential" any probe in this file looks for. Deliberately a
/// constant with an obvious shape: if it ever shows up in an unexpected place,
/// it is this test's, never the host's.
const FAKE_SECRET: &[u8] = b"SPIKE-FAKE-CRED-0123456789";

/// Budget for "a child should have reached its ready marker by now". Generous:
/// it guards against a hang, it does not measure anything.
const READY_BUDGET: Duration = Duration::from_secs(20);

// ---------------------------------------------------------------------------
// Environment reporting
// ---------------------------------------------------------------------------

/// Yama's ptrace scope, printed by every test that probes `ptrace` so a future
/// reader can tell what the environment was rather than guessing.
fn ptrace_scope() -> String {
    std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope")
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|error| format!("unreadable ({error})"))
}

fn report_environment(test: &str) {
    eprintln!(
        "[{test}] kernel={} yama/ptrace_scope={} uid={}",
        std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .unwrap_or_default()
            .trim(),
        ptrace_scope(),
        unsafe { libc::getuid() },
    );
}

// ---------------------------------------------------------------------------
// Low-level helpers
// ---------------------------------------------------------------------------

fn last_error() -> std::io::Error {
    std::io::Error::last_os_error()
}

/// A denial is `EPERM` **or** `EACCES` — measured, and the reason the ADR's
/// original "EPERM" wording was wrong: `ptrace` denial is `EPERM` while the
/// `/proc/<pid>/{mem,fd}` denial arrives as `EACCES` (the dumpable-zero inode is
/// owned by root, so the ordinary permission check refuses first).
fn is_denial(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EPERM) | Some(libc::EACCES))
}

fn pipe_pair() -> (OwnedFd, OwnedFd) {
    let mut fds = [0 as RawFd; 2];
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe: {}", last_error());
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

/// Wait for one readiness byte, or fail rather than hang.
fn await_ready(fd: &OwnedFd, what: &str) {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let rc = unsafe { libc::poll(&mut poll, 1, READY_BUDGET.as_millis() as libc::c_int) };
    assert!(rc > 0, "{what} never signalled readiness (poll rc={rc})");
    let mut byte = [0u8; 1];
    let read = unsafe { libc::read(fd.as_raw_fd(), byte.as_mut_ptr() as *mut libc::c_void, 1) };
    assert_eq!(read, 1, "{what} closed its readiness pipe without a byte");
}

/// Whether a pid names a process that has **not** exited.
///
/// Signal-based liveness is not enough, and this is the same trap the crate's own
/// `group_alive` fell into: `kill(pid, 0)` succeeds for a zombie, so a probe
/// built on it reports an exited adversary as a survivor. The first version of
/// this file did exactly that and claimed three leaks that had all been killed.
/// `/proc/<pid>/stat` is therefore the authority, with the signal probe kept only
/// to catch the `EPERM` case (a live process that is not ours to signal).
fn pid_alive(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(") ")
            .and_then(|(_, tail)| tail.split_whitespace().next())
            .map(|state| state != "Z")
            .unwrap_or(false),
        Err(_) => {
            (unsafe { libc::kill(pid, 0) }) != 0 && last_error().raw_os_error() == Some(libc::EPERM)
        }
    }
}

/// SIGKILL a pid and its process group, then wait until the kernel agrees it is
/// gone. Used by cleanup: no test may leave a stray behind.
fn kill_and_wait(pid: i32) {
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        // Reap if it is ours; harmless (ECHILD) if it is not.
        let mut status: libc::c_int = 0;
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if !pid_alive(pid) || Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Every process whose `/proc/<pid>/cmdline` mentions `marker`.
///
/// The marker is a per-run unique string that appears in every path this file
/// hands to a child, which is what makes the stray sweep exhaustive rather than
/// limited to the pids a test happened to record.
fn pids_matching(marker: &str) -> Vec<i32> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    let self_pid = std::process::id() as i32;
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        if pid == self_pid {
            continue;
        }
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if String::from_utf8_lossy(&cmdline).contains(marker) {
            found.push(pid);
        }
    }
    found
}

// ---------------------------------------------------------------------------
// Scratch directory + stray sweep
// ---------------------------------------------------------------------------

/// A per-test scratch directory whose path carries a unique marker, plus a
/// `Drop` that kills anything still referencing that marker.
///
/// The `Drop` is not tidiness: without it a panicking assertion would leave a
/// `sleep`-looping adversary on the host, and the next run would measure the
/// leftovers instead of itself.
struct Scratch {
    dir: PathBuf,
    marker: String,
}

impl Scratch {
    fn new(test: &str) -> Self {
        let marker = format!(
            "oabadv-{test}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let dir = std::env::temp_dir().join(&marker);
        std::fs::create_dir_all(&dir).expect("create scratch directory");
        Self { dir, marker }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn marker(&self) -> &str {
        &self.marker
    }

    /// Write an executable script.
    fn script(&self, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = self.path(name);
        std::fs::write(&path, body).expect("write script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("chmod script");
        path
    }

    fn read_when_present(&self, name: &str, budget: Duration) -> String {
        let path = self.path(name);
        let deadline = Instant::now() + budget;
        loop {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if !content.trim().is_empty() {
                    return content;
                }
            }
            assert!(
                Instant::now() < deadline,
                "{} never appeared (or stayed empty)",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn read_pid(&self, name: &str, budget: Duration) -> i32 {
        let raw = self.read_when_present(name, budget);
        raw.trim()
            .parse()
            .unwrap_or_else(|_| panic!("{name} does not contain a pid: {raw:?}"))
    }

    /// The end-of-test assertion: nothing this run started is still running.
    fn assert_no_strays(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let strays = pids_matching(&self.marker);
            if strays.is_empty() {
                return;
            }
            if Instant::now() >= deadline {
                panic!("processes from this test are still running: {strays:?}");
            }
            for pid in strays {
                kill_and_wait(pid);
            }
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for pid in pids_matching(&self.marker) {
            kill_and_wait(pid);
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ---------------------------------------------------------------------------
// 1. ptrace and /proc/<pid>/{mem,fd} — denial WITH a positive control
// ---------------------------------------------------------------------------

/// What one probe round measured against one victim.
struct ProbeRound {
    /// `Ok(bytes)` when `/proc/<pid>/mem` yielded the victim's page.
    mem: Result<Vec<u8>, std::io::Error>,
    /// `Ok(bytes)` when the victim's inherited pipe was re-opened through
    /// `/proc/<pid>/fd/<n>` and drained.
    fd: Result<Vec<u8>, std::io::Error>,
    /// `Ok(())` when `PTRACE_ATTACH` succeeded.
    ptrace: Result<(), std::io::Error>,
}

/// Fork a victim, probe it as its **direct parent**, and report what got through.
///
/// The parent/child relationship is held constant across both arms on purpose:
/// Yama at `ptrace_scope = 1` permits a parent to trace its own child, so the
/// only thing that differs between the arms is whether the product's containment
/// barrier ran. Anything Yama does, it does identically to both.
fn probe_victim(apply_barrier: bool) -> ProbeRound {
    // (a) A credential-bearing descriptor the victim inherits and never reads —
    //     the ADR's "inherited pipe via /proc/<pid>/fd" channel, with the bytes
    //     sitting in the pipe buffer.
    let (secret_read, secret_write) = pipe_pair();
    {
        let mut writer = std::fs::File::from(secret_write);
        writer.write_all(FAKE_SECRET).expect("seed the secret pipe");
    }
    let secret_fd = secret_read.as_raw_fd();

    // (b) A page the victim writes the secret into *after* forking, so the page
    //     read back through /proc/<pid>/mem is unambiguously the victim's own
    //     private copy and not a shared read-only mapping of this process.
    let mut slot = vec![0u8; FAKE_SECRET.len()];
    let slot_addr = slot.as_mut_ptr() as usize;

    let (ready_read, ready_write) = pipe_pair();
    let ready_write_fd = ready_write.as_raw_fd();

    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork: {}", last_error());
    if pid == 0 {
        // Victim. Async-signal-safe only (this is a fork of a threaded test
        // binary), and it never returns: `_exit` on any failure, `pause` forever
        // otherwise, so the parent decides when it dies.
        if apply_barrier {
            // The product code path — not a hand-rolled prctl. That is the whole
            // point: this asserts the *crate* establishes the barrier.
            match containment::establish_transient_secret_barrier() {
                Ok(ContainmentStatus::Active) => {}
                _ => unsafe { libc::_exit(91) },
            }
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                FAKE_SECRET.as_ptr(),
                slot_addr as *mut u8,
                FAKE_SECRET.len(),
            );
            libc::write(ready_write_fd, b"1".as_ptr() as *const libc::c_void, 1);
            loop {
                libc::pause();
            }
        }
    }

    drop(ready_write);
    await_ready(&ready_read, "victim");

    let mem = read_proc_mem(pid, slot_addr, FAKE_SECRET.len());
    let fd = read_proc_fd(pid, secret_fd);
    let ptrace = try_ptrace_attach(pid);

    kill_and_wait(pid);
    // Keep the slot alive until after the probe; dropping it earlier would
    // unmap the address we just asked the kernel about.
    drop(slot);
    drop(secret_read);
    ProbeRound { mem, fd, ptrace }
}

fn read_proc_mem(pid: i32, addr: usize, len: usize) -> Result<Vec<u8>, std::io::Error> {
    let file = std::fs::File::open(format!("/proc/{pid}/mem"))?;
    let mut buf = vec![0u8; len];
    let read = unsafe {
        libc::pread(
            file.as_raw_fd(),
            buf.as_mut_ptr() as *mut libc::c_void,
            len,
            addr as libc::off_t,
        )
    };
    if read < 0 {
        return Err(last_error());
    }
    buf.truncate(read as usize);
    Ok(buf)
}

fn read_proc_fd(pid: i32, fd: RawFd) -> Result<Vec<u8>, std::io::Error> {
    let mut file = std::fs::File::open(format!("/proc/{pid}/fd/{fd}"))?;
    let mut buf = vec![0u8; 256];
    let read = file.read(&mut buf)?;
    buf.truncate(read);
    Ok(buf)
}

fn try_ptrace_attach(pid: i32) -> Result<(), std::io::Error> {
    let rc = unsafe { libc::ptrace(libc::PTRACE_ATTACH, pid, 0, 0) };
    if rc != 0 {
        return Err(last_error());
    }
    // Attached: collect the SIGSTOP-stop, then let go. Bounded so a surprise
    // cannot hang the suite.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut status: libc::c_int = 0;
        let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if waited == pid || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    unsafe { libc::ptrace(libc::PTRACE_DETACH, pid, 0, 0) };
    Ok(())
}

#[test]
#[ignore = "forks real processes and probes /proc"]
fn ptrace_and_proc_probes_are_denied_only_because_of_the_barrier() {
    report_environment("ptrace-and-proc");

    // Positive control first: without the barrier the credential is genuinely
    // readable through all three channels. If this arm ever stops succeeding,
    // the denial arm below proves nothing and the test says so here.
    let control = probe_victim(false);
    let control_mem = control
        .mem
        .as_ref()
        .expect("positive control: /proc/<pid>/mem must be readable at dumpable=1");
    assert_eq!(
        control_mem.as_slice(),
        FAKE_SECRET,
        "positive control: the credential must actually be recoverable from the victim's memory"
    );
    let control_fd = control
        .fd
        .as_ref()
        .expect("positive control: /proc/<pid>/fd must be readable at dumpable=1");
    assert_eq!(
        control_fd.as_slice(),
        FAKE_SECRET,
        "positive control: the inherited descriptor must leak the credential at dumpable=1"
    );
    control
        .ptrace
        .as_ref()
        .expect("positive control: a parent must be able to PTRACE_ATTACH its child at dumpable=1");

    // Same relationship, same probes, barrier applied by the crate.
    let contained = probe_victim(true);
    for (what, outcome) in [
        ("/proc/<pid>/mem", contained.mem.map(|bytes| bytes.len())),
        ("/proc/<pid>/fd", contained.fd.map(|bytes| bytes.len())),
        ("ptrace", contained.ptrace.map(|()| 0)),
    ] {
        match outcome {
            Ok(_) => panic!(
                "{what} succeeded against a contained runtime: the same-UID barrier does not hold"
            ),
            Err(error) => {
                assert!(
                    is_denial(&error),
                    "{what} must be denied with EPERM or EACCES, got {error:?}"
                );
                eprintln!("[ptrace-and-proc] {what} denied: {error}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 2. Dumpability regression guards
// ---------------------------------------------------------------------------

/// `PR_GET_DUMPABLE` readback plus the reversion case, run in a forked child.
///
/// Deliberately not run in the test process: setting this process
/// non-dumpable would silently invalidate the positive control in test 1 if the
/// two ever ran concurrently. A forked child keeps the measurement hermetic.
#[test]
#[ignore = "forks a real process"]
fn the_dumpable_guard_reads_back_zero_and_rejects_a_reversion() {
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork: {}", last_error());
    if pid == 0 {
        let code = unsafe {
            if !matches!(
                containment::establish_transient_secret_barrier(),
                Ok(ContainmentStatus::Active)
            ) {
                libc::_exit(11);
            }
            // The barrier must be observable through the kernel, not just
            // through a return value.
            if libc::prctl(libc::PR_GET_DUMPABLE) != 0 {
                libc::_exit(12);
            }
            // And the crate's own regression guard must agree.
            if containment::assert_dumpable_zero().is_err() {
                libc::_exit(13);
            }
            // Now simulate the failure this guard exists for: a dependency that
            // calls prctl(PR_SET_DUMPABLE, 1) behind our back. The guard must
            // turn that into a test failure rather than a silent collapse of the
            // containment model.
            if libc::prctl(libc::PR_SET_DUMPABLE, 1) != 0 {
                libc::_exit(14);
            }
            if containment::assert_dumpable_zero().is_ok() {
                libc::_exit(15);
            }
            0
        };
        unsafe { libc::_exit(code) };
    }

    let status = wait_for_exit(pid, Duration::from_secs(10));
    assert_eq!(
        status, 0,
        "dumpability guard child failed (see the exit-code map in the test body)"
    );
}

/// Block until `pid` exits, returning its exit code. Never waits forever.
fn wait_for_exit(pid: i32, budget: Duration) -> i32 {
    let deadline = Instant::now() + budget;
    loop {
        let mut status: libc::c_int = 0;
        let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if waited == pid {
            if libc::WIFEXITED(status) {
                return libc::WEXITSTATUS(status);
            }
            panic!("child did not exit normally (status {status})");
        }
        if Instant::now() >= deadline {
            kill_and_wait(pid);
            panic!("child did not exit within {budget:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---------------------------------------------------------------------------
// The shipped binary, spawned for real
// ---------------------------------------------------------------------------

/// A delivered `[pty]` projection. Loopback listener, because the fail-closed
/// listener guard is not what these tests are measuring.
fn projection(port: u16, command: &Path, verifier: &str) -> String {
    format!(
        r#"[pty]
enabled = true
listen = "127.0.0.1:{port}"
tls_terminated_upstream = true
command = "{}"
max_sessions = 4
absolute_session_ttl = "12h"
scrollback_kib = 1024
scrollback_replay = false
admin_credential_hash = "{verifier}"
"#,
        command.display()
    )
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

/// The shipped runtime, running as a direct child of this test.
///
/// Direct-child is required, not incidental: it is the relationship Yama permits
/// at `ptrace_scope = 1`, so a denied probe against it cannot be blamed on Yama.
struct Runtime {
    child: std::process::Child,
    port: u16,
    credential: String,
    log: Arc<std::sync::Mutex<String>>,
}

impl Runtime {
    fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    fn admin(&self, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
        http(self.port, method, path, Some(&self.credential), body)
    }

    fn log(&self) -> String {
        self.log.lock().expect("log mutex").clone()
    }

    fn shutdown(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Drain the child's stdout **and** stderr on threads so neither can fill its
/// pipe and neither is lost when a test fails: the runtime's own log is the first
/// thing worth reading when one of these tests misbehaves.
///
/// Both streams are captured because `tracing_subscriber::fmt` writes to stdout
/// while `anyhow`'s startup refusal goes to stderr — draining only one of them
/// left the port discovery below staring at an empty buffer for twenty seconds.
fn capture_output(child: &mut std::process::Child) -> Arc<std::sync::Mutex<String>> {
    let log = Arc::new(std::sync::Mutex::new(String::new()));
    let mut streams: Vec<Box<dyn Read + Send>> = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        streams.push(Box::new(stdout));
    }
    if let Some(stderr) = child.stderr.take() {
        streams.push(Box::new(stderr));
    }
    for mut stream in streams {
        let sink = log.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(read) = stream.read(&mut buf) {
                if read == 0 {
                    return;
                }
                sink.lock()
                    .expect("log mutex")
                    .push_str(&String::from_utf8_lossy(&buf[..read]));
            }
        });
    }
    log
}

/// Spawn `openab-pty` with a delivered projection, optionally under a seccomp
/// filter that makes `prctl(PR_SET_DUMPABLE, ...)` fail.
fn spawn_runtime_process(config: &Path, block_set_dumpable: bool) -> std::process::Child {
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_openab-pty"));
    command
        .arg("--config")
        .arg(config)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if block_set_dumpable {
        use std::os::unix::process::CommandExt;
        // Safety: the closure runs between fork and execv and calls only
        // `prctl`, which is async-signal-safe and allocation-free here. The
        // filter survives `execv` because `PR_SET_NO_NEW_PRIVS` is set with it.
        unsafe {
            command.pre_exec(install_set_dumpable_blocking_filter);
        }
    }
    command.spawn().expect("spawn openab-pty")
}

/// Start a runtime and wait until it reports the port it actually bound.
///
/// The port comes from the runtime's log rather than from a port this test picked
/// first, because picking one leaves a window in which something else — including
/// another test in this file — can take it. That race was real: a runtime that
/// lost it exited with `Address already in use` while the probe loop happily
/// connected to the *other* test's listener and then measured the wrong process.
fn start_runtime(scratch: &Scratch, command: &Path) -> Runtime {
    let generated = AdminAuthenticator::generate();
    let credential =
        String::from_utf8(generated.plaintext.as_bytes().to_vec()).expect("hex credential");
    let config = scratch.path("config.toml");
    std::fs::write(&config, projection(0, command, &generated.verifier))
        .expect("write projection");
    // Validated here too, so a bad projection fails as a test error rather than
    // as an opaque "the runtime did not come up".
    config::validate_projection_file(&config).expect("the test projection must validate");

    let mut child = spawn_runtime_process(&config, false);
    let log = capture_output(&mut child);
    let deadline = Instant::now() + READY_BUDGET;
    let port = loop {
        if let Some(port) = listening_port_from_log(&log.lock().expect("log mutex")) {
            break port;
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!(
                "openab-pty exited before listening ({status}):\n{}",
                log.lock().expect("log mutex")
            );
        }
        assert!(
            Instant::now() < deadline,
            "openab-pty never reported a listening address:\n{}",
            log.lock().expect("log mutex")
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    // The log line is emitted after `bind`, so a connection must now succeed.
    std::net::TcpStream::connect(("127.0.0.1", port)).unwrap_or_else(|error| {
        panic!("runtime reported port {port} but refused a connection: {error}")
    });
    Runtime {
        child,
        port,
        credential,
        log,
    }
}

/// Pull the bound port out of the runtime's own startup log. The log is
/// ANSI-decorated, so this looks for the address rather than a `key=value` shape.
fn listening_port_from_log(log: &str) -> Option<u16> {
    let start = log.find("127.0.0.1:")? + "127.0.0.1:".len();
    let digits: String = log[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok().filter(|port| *port != 0)
}

/// Minimal blocking HTTP/1.1 client. `Connection: close` makes the read end at
/// EOF, so a malformed response is a failure rather than a hang.
fn http(
    port: u16,
    method: &str,
    path: &str,
    credential: Option<&str>,
    body: Option<&str>,
) -> (u16, String) {
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    if let Some(credential) = credential {
        request.push_str(&format!("Authorization: Bearer {credential}\r\n"));
    }
    match body {
        Some(body) => {
            request.push_str("Content-Type: application/json\r\n");
            request.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
            request.push_str(body);
        }
        None => request.push_str("\r\n"),
    }
    let mut stream =
        std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to the runtime");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("read timeout");
    stream
        .write_all(request.as_bytes())
        .expect("write the request");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read the response");
    let status = raw
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    (status, raw)
}

/// Owner of `/proc/<pid>/fd`. The kernel reassigns these inodes to root the
/// moment a process becomes non-dumpable, which makes this a second, independent
/// observation of the barrier — and one that works on a process we did not fork.
///
/// `/proc/<pid>` **itself** is deliberately exempt from that rule (its mode is
/// exactly `dr-xr-xr-x`, which the kernel special-cases so `stat /proc/pid`
/// keeps reporting the process's euid for the benefit of `procps`), so probing
/// the directory rather than a file inside it would have reported a dumpable
/// process. Measured, not assumed: the first version of this test did exactly
/// that and read `1000` for a runtime whose `/proc/<pid>/mem` was already denied.
fn proc_fd_dir_owner(pid: i32) -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(format!("/proc/{pid}/fd"))
        .unwrap_or_else(|error| panic!("stat /proc/{pid}/fd: {error}"))
        .uid()
}

fn open_proc_mem(pid: i32) -> Result<std::fs::File, std::io::Error> {
    std::fs::File::open(format!("/proc/{pid}/mem"))
}

#[test]
#[ignore = "spawns the built binary and probes /proc"]
fn the_shipped_runtime_serves_with_dumpability_already_off() {
    report_environment("shipped-runtime");
    let scratch = Scratch::new("shipped");

    // The positive control is a sibling child of this test with the same UID and
    // the same parent: everything Yama looks at is identical, and only the
    // barrier differs.
    let control_script = scratch.script("control.sh", "#!/bin/sh\nwhile : ; do sleep 5; done\n");
    let mut control = std::process::Command::new(&control_script)
        .spawn()
        .expect("spawn the dumpable control child");
    let control_pid = control.id() as i32;

    let session_command = scratch.script("session.sh", "#!/bin/sh\nwhile : ; do sleep 5; done\n");
    let runtime = start_runtime(&scratch, &session_command);
    let runtime_pid = runtime.pid();

    // Serving, not merely alive: a runtime that failed to start would trivially
    // "pass" a denial-only assertion.
    let (status, _) = runtime.admin("GET", "/admin/sessions", None);
    assert_eq!(status, 200, "the runtime must be serving the admin plane");

    let control_owner = proc_fd_dir_owner(control_pid);
    let runtime_owner = proc_fd_dir_owner(runtime_pid);
    eprintln!(
        "[shipped-runtime] /proc/<pid>/fd owner: control={control_owner} runtime={runtime_owner} (uid={})",
        unsafe { libc::getuid() }
    );
    assert_eq!(
        control_owner,
        unsafe { libc::getuid() },
        "positive control: an ordinary dumpable child's /proc/<pid>/fd belongs to its own uid"
    );
    assert_eq!(
        runtime_owner, 0,
        "the shipped runtime's /proc/<pid>/fd must be root-owned, which only happens once \
         PR_SET_DUMPABLE=0 has been applied"
    );

    // And the direct consequence, probed the same way for both processes.
    open_proc_mem(control_pid)
        .expect("positive control: a dumpable child's /proc/<pid>/mem is openable by its parent");
    match open_proc_mem(runtime_pid) {
        Ok(_) => {
            panic!("/proc/<runtime>/mem is openable: the transient-secret barrier is not live")
        }
        Err(error) => {
            assert!(
                is_denial(&error),
                "the denial must be EPERM or EACCES, got {error:?}"
            );
            eprintln!("[shipped-runtime] /proc/<runtime>/mem denied: {error}");
        }
    }

    runtime.shutdown();
    let _ = control.kill();
    let _ = control.wait();
    scratch.assert_no_strays();
}

// ---------------------------------------------------------------------------
// 3. `prctl` failure is fail-closed
// ---------------------------------------------------------------------------

// A real seccomp filter is installed rather than a mocked failure path, because
// what the ADR claims is that the *runtime refuses to serve* when the kernel
// will not give it the barrier — and an unprivileged process can prove that for
// real: `PR_SET_NO_NEW_PRIVS` followed by `PR_SET_SECCOMP` needs no capability,
// and the filter survives `execv`. So the shipped binary is started under a
// kernel that genuinely denies `prctl(PR_SET_DUMPABLE, ...)`, with no test seam
// involved.

const AUDIT_ARCH: Option<u32> = if cfg!(target_arch = "x86_64") {
    Some(0xC000_003E)
} else if cfg!(target_arch = "aarch64") {
    Some(0xC000_00B7)
} else {
    None
};

/// Classic BPF: allow everything except `prctl(PR_SET_DUMPABLE, …)`, which
/// returns `EPERM`.
fn set_dumpable_blocking_program() -> [libc::sock_filter; 8] {
    const LD_W_ABS: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
    const JEQ_K: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
    const RET_K: u16 = 0x06; // BPF_RET | BPF_K
    const RET_ERRNO_EPERM: u32 = 0x0005_0000 | (libc::EPERM as u32 & 0xffff);
    const RET_ALLOW: u32 = 0x7fff_0000;
    // Offsets into `struct seccomp_data`: nr = 0, arch = 4, args[0] = 16.
    const OFF_NR: u32 = 0;
    const OFF_ARCH: u32 = 4;
    const OFF_ARG0: u32 = 16;

    let arch = AUDIT_ARCH.unwrap_or(0);
    let instruction = |code: u16, jt: u8, jf: u8, k: u32| libc::sock_filter { code, jt, jf, k };
    [
        instruction(LD_W_ABS, 0, 0, OFF_ARCH),
        // Foreign architecture: allow, so a mismatch degrades to a no-op filter
        // rather than a mystery failure.
        instruction(JEQ_K, 0, 5, arch),
        instruction(LD_W_ABS, 0, 0, OFF_NR),
        instruction(JEQ_K, 0, 3, libc::SYS_prctl as u32),
        instruction(LD_W_ABS, 0, 0, OFF_ARG0),
        instruction(JEQ_K, 0, 1, libc::PR_SET_DUMPABLE as u32),
        instruction(RET_K, 0, 0, RET_ERRNO_EPERM),
        instruction(RET_K, 0, 0, RET_ALLOW),
    ]
}

/// Install the filter on the calling process. Irreversible, so this only ever
/// runs in a forked child or a pre-`exec` hook — never in the test process.
fn install_set_dumpable_blocking_filter() -> std::io::Result<()> {
    let program = set_dumpable_blocking_program();
    let prog = libc::sock_fprog {
        len: program.len() as u16,
        filter: program.as_ptr() as *mut libc::sock_filter,
    };
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(last_error());
        }
        if libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &prog as *const libc::sock_fprog,
        ) != 0
        {
            return Err(last_error());
        }
    }
    Ok(())
}

/// Wait for a spawned runtime to exit, then collect its output. Never hangs.
fn wait_with_output(mut child: std::process::Child, budget: Duration) -> (Option<i32>, String) {
    let deadline = Instant::now() + budget;
    let mut code = None;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                code = status.code();
                break;
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                break;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    let output = child.wait_with_output().expect("collect output");
    let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    (code, text)
}

#[test]
#[ignore = "installs a seccomp filter in child processes and spawns the built binary"]
fn a_runtime_that_cannot_set_dumpable_refuses_to_serve_before_reading_a_credential() {
    let Some(_) = AUDIT_ARCH else {
        eprintln!(
            "SKIP a_runtime_that_cannot_set_dumpable_refuses_to_serve: no AUDIT_ARCH for {}",
            std::env::consts::ARCH
        );
        return;
    };
    let scratch = Scratch::new("failclosed");

    // (a) The filter is real. Without this control, every assertion below could
    //     be satisfied by a filter that silently failed to install.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork: {}", last_error());
    if pid == 0 {
        let code = unsafe {
            if install_set_dumpable_blocking_filter().is_err() {
                libc::_exit(21);
            }
            if libc::prctl(libc::PR_SET_DUMPABLE, 0) == 0 {
                libc::_exit(22); // the filter is not in effect
            }
            if last_error().raw_os_error() != Some(libc::EPERM) {
                libc::_exit(23);
            }
            // The crate's barrier must surface that as an error, not swallow it.
            if containment::establish_transient_secret_barrier().is_ok() {
                libc::_exit(24);
            }
            0
        };
        unsafe { libc::_exit(code) };
    }
    assert_eq!(
        wait_for_exit(pid, Duration::from_secs(10)),
        0,
        "seccomp control child failed (see the exit-code map in the test body)"
    );

    let session_command = scratch.script("session.sh", "#!/bin/sh\nwhile : ; do sleep 5; done\n");
    let generated = AdminAuthenticator::generate();
    let port = free_port();

    // (b) A *valid* projection: the only reason to refuse is the barrier.
    let good = scratch.path("good.toml");
    std::fs::write(&good, projection(port, &session_command, &generated.verifier))
        .expect("write projection");
    let (code, output) = wait_with_output(spawn_runtime_process(&good, true), READY_BUDGET);
    assert_ne!(code, Some(0), "the runtime must not exit successfully");
    assert!(
        output.contains("PR_SET_DUMPABLE") && output.contains("refusing to start"),
        "the refusal must name the barrier it could not establish: {output}"
    );
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
        "a runtime that refused to start must not have bound its listener"
    );

    // (c) A *poisoned* projection under the same filter. If the runtime read the
    //     credential-bearing input before establishing containment, we would see
    //     the projection rejection instead of the containment refusal.
    let poisoned_body = projection(port, &session_command, &generated.verifier)
        .replace(&generated.verifier, "${secrets.pty_admin_hash}");
    let poisoned = scratch.path("poisoned.toml");
    std::fs::write(&poisoned, &poisoned_body).expect("write poisoned projection");
    let (code, output) = wait_with_output(spawn_runtime_process(&poisoned, true), READY_BUDGET);
    assert_ne!(code, Some(0));
    assert!(
        output.contains("PR_SET_DUMPABLE"),
        "containment must be refused first: {output}"
    );
    assert!(
        !output.contains("invalid PTY projection"),
        "the projection must not have been read before the refusal: {output}"
    );

    // (d) Ordering control: the same poison, no filter. This must fail *at the
    //     projection*, which proves (c)'s silence about the projection is an
    //     ordering fact and not an undetectable poison.
    let (code, output) = wait_with_output(spawn_runtime_process(&poisoned, false), READY_BUDGET);
    assert_ne!(code, Some(0));
    assert!(
        output.contains("invalid PTY projection"),
        "ordering control: without the filter the poisoned projection must be what fails: {output}"
    );
    assert!(
        !output.contains("refusing to start: could not establish"),
        "ordering control: the barrier must have succeeded here: {output}"
    );

    scratch.assert_no_strays();
}

// ---------------------------------------------------------------------------
// 4. No privileged admin path from inside a managed session
//
// What is asserted is *not* unreachability. A managed session shares the
// runtime's network namespace, so it can connect to the one listener and does —
// the probe below gets an HTTP response. The property is that being inside
// authorizes nothing: there is no admin-only socket (TCP or UNIX) to find, and
// the request that does arrive is refused for want of the credential.
// ---------------------------------------------------------------------------

/// Every AF_UNIX socket path in this network namespace.
fn unix_socket_paths() -> std::collections::BTreeSet<String> {
    let mut found = std::collections::BTreeSet::new();
    let Ok(content) = std::fs::read_to_string("/proc/net/unix") else {
        return found;
    };
    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 8 {
            found.insert(fields[7].to_string());
        }
    }
    found
}

/// Every listening TCP port in this network namespace.
fn listening_tcp_ports() -> std::collections::BTreeSet<u16> {
    let mut found = std::collections::BTreeSet::new();
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in content.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // Field 3 is the connection state; `0A` is TCP_LISTEN.
            if fields.len() < 10 || fields[3] != "0A" {
                continue;
            }
            if let Some((_, port)) = fields[1].split_once(':') {
                if let Ok(port) = u16::from_str_radix(port, 16) {
                    found.insert(port);
                }
            }
        }
    }
    found
}

#[test]
#[ignore = "spawns the built binary, binds a socket, and runs a probe inside a managed session"]
fn a_managed_session_can_reach_the_listener_but_is_refused_admin() {
    let scratch = Scratch::new("adminsock");
    let out = scratch.path("probe.out");
    let port_file = scratch.path("port");

    // The probe runs as the session's own program — a child of the runtime, in
    // the runtime's container — which is exactly the position the ADR's
    // admin-plane claim is about. It is handed the port because an adversary in
    // that position could read it out of /proc/net/tcp anyway; withholding it
    // would weaken the test, not the model.
    let probe = scratch.script(
        "session-probe.sh",
        &format!(
            r#"#!/bin/bash
PORT=$(cat {port_file})
{{
  if exec 3<>/dev/tcp/127.0.0.1/$PORT; then
    printf 'GET /admin/sessions HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n' >&3
    echo "admin_status=$(head -n 1 <&3 | tr -d '\r')"
    exec 3<&-
  else
    echo "admin_status=connect-failed"
  fi
  echo "unix_sockets_matching=$(grep -c -i -e openab -e '\-pty' /proc/net/unix)"
  echo "socket_files=$(find /run /tmp -maxdepth 3 -type s 2>/dev/null | tr '\n' ' ')"
  echo "run_dir=$(ls -A /run/openab-pty 2>&1 | tr '\n' ' ')"
  echo "probe_complete=1"
}} > "{out}" 2>&1
while : ; do sleep 5; done
"#,
            port_file = port_file.display(),
            out = out.display()
        ),
    );

    // Baseline for the diff below. Enumerating the runtime's own `/proc/<pid>/fd`
    // is not available here — and that unavailability is the barrier from test 2
    // working as designed — so "which sockets did this runtime open" is answered
    // by what appeared in the network namespace while it ran.
    let tcp_before = listening_tcp_ports();
    let unix_before = unix_socket_paths();

    let runtime = start_runtime(&scratch, &probe);
    let runtime_pid = runtime.pid();
    let port = runtime.port;
    std::fs::write(&port_file, port.to_string()).expect("publish the port to the probe");

    // (a) Recorded, not asserted: the barrier from test 2 is what makes external
    //     fd enumeration unavailable, and asserting it a second time here would
    //     put a second test's claim in this one.
    eprintln!(
        "[adminsock] /proc/{runtime_pid}/fd enumerable={:?} owner={:?}",
        std::fs::read_dir(format!("/proc/{runtime_pid}/fd")).map(|entries| entries.count()),
        std::fs::metadata(format!("/proc/{runtime_pid}/fd")).map(|meta| {
            use std::os::unix::fs::MetadataExt;
            (meta.uid(), format!("{:o}", meta.mode()))
        })
    );

    // (b) From inside a managed session: creating the session runs the probe.
    let (status, body) = runtime.admin(
        "POST",
        "/admin/sessions",
        Some(r#"{"name":"probe","rows":24,"cols":80}"#),
    );
    assert_eq!(
        status,
        200,
        "create failed: {body}\nruntime log:\n{}",
        runtime.log()
    );

    let report = scratch.read_when_present("probe.out", Duration::from_secs(30));
    eprintln!("[adminsock] in-session probe:\n{report}");

    // (c) Everything this runtime opened, listener and session included.
    let new_tcp: Vec<u16> = listening_tcp_ports()
        .difference(&tcp_before)
        .copied()
        .collect();
    let new_unix: Vec<String> = unix_socket_paths()
        .difference(&unix_before)
        .cloned()
        .collect();
    eprintln!("[adminsock] new listening tcp={new_tcp:?} new unix sockets={new_unix:?}");
    assert_eq!(
        new_tcp,
        vec![port],
        "the runtime must listen on exactly its configured port: no separate loopback admin \
         listener may appear, before or after a session exists"
    );
    assert!(
        !new_unix
            .iter()
            .any(|path| path.contains("openab") || path.contains("pty")),
        "a UNIX socket belonging to the PTY runtime appeared: {new_unix:?}"
    );
    if let Ok(entries) = std::fs::read_dir("/run/openab-pty") {
        for entry in entries.flatten() {
            let kind = entry.file_type().expect("file type");
            assert!(
                !std::os::unix::fs::FileTypeExt::is_socket(&kind),
                "a socket appeared under /run/openab-pty: {:?}",
                entry.path()
            );
        }
    }
    assert!(
        report.contains("probe_complete=1"),
        "the in-session probe did not finish: {report}"
    );
    assert!(
        report.contains("admin_status=HTTP/1.1 401 Unauthorized"),
        "the request from inside a managed session must arrive and be refused: the listener is \
         reachable, the credential is the boundary, and locality authorizes nothing: {report}"
    );
    assert!(
        report.contains("unix_sockets_matching=0"),
        "no openab/pty UNIX socket may be visible from inside a session: {report}"
    );
    let socket_files = report
        .lines()
        .find_map(|line| line.strip_prefix("socket_files="))
        .expect("socket_files line");
    assert!(
        !socket_files.to_ascii_lowercase().contains("openab"),
        "a socket file belonging to the runtime is reachable from the session: {socket_files}"
    );

    let (status, _) = runtime.admin("DELETE", "/admin/sessions/probe", None);
    assert_eq!(status, 200, "kill the probe session");
    runtime.shutdown();
    scratch.assert_no_strays();
}

// ---------------------------------------------------------------------------
// 5. Teardown against the three adversaries
// ---------------------------------------------------------------------------

/// A manager wired exactly as the binary wires it: the real `portable-pty`
/// spawner, the real kill domain with the tier that startup detection selects,
/// and the policy the delivered projection asks for.
fn adversary_manager(scratch: &Scratch, command: &Path) -> (Arc<SessionManager>, Arc<KillDomain>) {
    let audit = AuditLogger;
    let generated = AdminAuthenticator::generate();
    let text = projection(free_port(), command, &generated.verifier);
    let parsed = config::validate_projection(&text).expect("projection must validate");
    let spawner = Arc::new(PortablePtySpawner);
    let kill = Arc::new(KillDomain::new(TrackingLimits::default(), audit.clone()));
    eprintln!("[{}] {}", scratch.marker(), kill.tier().describe());
    let policy = SessionPolicy {
        // A short grace keeps the SIGTERM→SIGKILL escalation observable without
        // making the suite wait out the 5s default. The grace is a constant, not
        // a projection knob, so it is set here rather than in the TOML.
        teardown_grace: Duration::from_secs(1),
        ..SessionPolicy::from_config(&parsed)
    };
    let manager = SessionManager::new(
        parsed,
        policy,
        TokenStore::with_default_ttl(audit.clone()),
        audit,
        kill.clone(),
        spawner,
    )
    .expect("session manager");
    (manager, kill)
}

fn wait_until_gone(pid: i32, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    !pid_alive(pid)
}

/// Every process in `pgid`, as `(pid, state, ppid)` read from `/proc/<pid>/stat`.
///
/// `kill(-pgid, 0)` cannot tell a live member from a zombie one, and that
/// distinction decides whether a teardown leaked. This is the diagnostic that
/// settles it, and it exists because the first run of these tests reported the
/// session leader as a survivor while the leader was already gone.
fn group_members(pgid: i32) -> Vec<(i32, char, i32)> {
    let mut members = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return members;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `comm` can contain spaces and parentheses, so parse after the last ')'.
        let Some(tail) = stat.rsplit_once(") ").map(|(_, tail)| tail) else {
            continue;
        };
        let fields: Vec<&str> = tail.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        let state = fields[0].chars().next().unwrap_or('?');
        let ppid = fields[1].parse::<i32>().unwrap_or(0);
        if fields[2].parse::<i32>().ok() == Some(pgid) {
            members.push((pid, state, ppid));
        }
    }
    members
}

/// Run one adversary through a real session teardown and assert Tier 1's
/// documented semantics. Tier 1 is the only kill domain the runtime implements,
/// and its contract is best effort: survivors it can see are reported, audited,
/// and counted; one that left the process group is invisible by construction.
async fn teardown_case(label: &str, scratch: &Scratch, command: &Path) {
    let (manager, kill) = adversary_manager(scratch, command);
    let name = SessionName::parse("adversary").expect("session name");
    manager
        .create(name.clone(), WindowSize::default())
        .expect("create the session");

    let leader = scratch.read_pid("leader.pid", READY_BUDGET);
    let adversary = scratch.read_pid("adversary.pid", READY_BUDGET);
    assert!(pid_alive(leader) && pid_alive(adversary));

    let report = tokio::time::timeout(Duration::from_secs(30), manager.kill(&name))
        .await
        .expect("teardown must be bounded, never blockable")
        .expect("kill");
    let outcome = &report.kill_domain;
    eprintln!(
        "[{label}] tier={} best_effort={} survivors={:?} elapsed={}ms leader={leader} adversary={adversary}",
        outcome.tier.label(),
        outcome.is_best_effort(),
        outcome.survivors,
        outcome.elapsed_ms
    );
    eprintln!(
        "[{label}] post-teardown members of pgid {leader} (pid, state, ppid): {:?}",
        group_members(leader)
    );

    assert_eq!(outcome.tier, kill.tier());
    assert!(
        outcome.is_best_effort(),
        "tier 1 must never claim a hard guarantee"
    );
    for pid in &outcome.survivors {
        assert!(
            pid_alive(*pid),
            "{pid} is reported as a survivor but is not alive: a reaped child is not a leak, and \
             counting one makes the leak metric fire unconditionally"
        );
    }
    assert_eq!(
        kill.leaked_process_count(),
        outcome.survivors.len() as u64,
        "every reported survivor must be counted on the leak metric"
    );
    assert!(
        wait_until_gone(leader, Duration::from_secs(5)),
        "the session leader must be dead after teardown"
    );

    if pid_alive(adversary) {
        // Best effort, stated as the ADR states it: a survivor the runtime can
        // see must be reported and counted; one that left the process group is
        // invisible by construction, and the pod or task replacement is its
        // documented reclamation path. Assert the disjunction and print which
        // branch was measured, so a future improvement to discovery does not have
        // to fight this test.
        if outcome.survivors.contains(&adversary) {
            assert!(
                kill.leaked_process_count() >= 1,
                "a detected survivor must be counted on the leak metric"
            );
            eprintln!("[{label}] survivor DETECTED, audited, and counted: {adversary}");
        } else {
            eprintln!(
                "[{label}] survivor NOT discoverable by the runtime (left the process group; \
                 reclaimed only when the pod or task is replaced): {adversary}"
            );
        }
    } else {
        eprintln!("[{label}] the tier-1 process-group escalation reached the adversary");
    }

    // Clean up whatever survived, then prove nothing of ours is left running.
    kill_and_wait(adversary);
    manager.shutdown().await;
}

fn looping_script(body: &str) -> String {
    format!("#!/bin/sh\n{body}\nwhile : ; do sleep 5; done\n")
}

#[tokio::test]
#[ignore = "spawns real PTY children and adversary processes"]
async fn teardown_against_a_setsid_escapee() {
    let scratch = Scratch::new("setsid");
    assert!(
        Path::new("/usr/bin/setsid").exists() || Path::new("/bin/setsid").exists(),
        "this test needs setsid(1) to build the escapee"
    );
    let escapee = scratch.script(
        "escapee.sh",
        &looping_script(&format!(
            "echo $$ > {}",
            scratch.path("adversary.pid").display()
        )),
    );
    // The shell forks first and `setsid` runs in that child: a process-group
    // leader calling setsid(2) itself gets EPERM, so the escape has to be made
    // this way round.
    let command = scratch.script(
        "command.sh",
        &looping_script(&format!(
            "echo $$ > {}\nsetsid {} &",
            scratch.path("leader.pid").display(),
            escapee.display()
        )),
    );
    teardown_case("setsid-escapee", &scratch, &command).await;
    scratch.assert_no_strays();
}

#[tokio::test]
#[ignore = "spawns real PTY children and adversary processes"]
async fn teardown_against_a_double_fork_whose_intermediate_exits_first() {
    let scratch = Scratch::new("doublefork");
    let orphan = scratch.script(
        "orphan.sh",
        &looping_script(&format!(
            "echo $$ > {}",
            scratch.path("adversary.pid").display()
        )),
    );
    // The intermediate runs in the foreground and exits before the leader
    // records itself, so by the time anything could scan, the orphan has already
    // reparented and its lineage is unrecoverable — the spike's F1 case.
    let intermediate = scratch.script(
        "intermediate.sh",
        &format!("#!/bin/sh\n{} &\nexit 0\n", orphan.display()),
    );
    let command = scratch.script(
        "command.sh",
        &looping_script(&format!(
            "{}\necho $$ > {}",
            intermediate.display(),
            scratch.path("leader.pid").display()
        )),
    );
    teardown_case("double-fork-orphan", &scratch, &command).await;
    scratch.assert_no_strays();
}

#[tokio::test]
#[ignore = "spawns real PTY children and adversary processes"]
async fn teardown_against_a_sigterm_trapping_child() {
    let scratch = Scratch::new("stubborn");
    let stubborn = scratch.script(
        "stubborn.sh",
        &looping_script(&format!(
            "trap '' TERM\necho $$ > {}",
            scratch.path("adversary.pid").display()
        )),
    );
    let command = scratch.script(
        "command.sh",
        &looping_script(&format!(
            "echo $$ > {}\n{} &",
            scratch.path("leader.pid").display(),
            stubborn.display()
        )),
    );
    teardown_case("sigterm-trapping", &scratch, &command).await;
    scratch.assert_no_strays();
}

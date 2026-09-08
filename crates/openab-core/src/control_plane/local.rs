//! Local IPC surface for driving the primary side of the control-plane client.
//!
//! An out-of-process caller — a CLI subcommand, an MCP tool, an operator
//! script — needs to spawn/await/cancel delegations and list peers, but the
//! socket to the CP is owned by one in-process serve loop. This module bridges
//! the two with a Unix-domain socket that speaks newline-delimited JSON: one
//! [`LocalRequest`] per line in, one [`LocalResponse`] per line out.
//!
//! ## Security
//!
//! The socket is a local trust boundary: anything that can connect to it can
//! initiate delegations under this runtime's CP identity. So the socket is
//! created **owner-only** (`0600`) inside an **owner-only** parent directory
//! (`0700`), and the default path lives under the invoking user's home. There
//! is no authentication beyond filesystem permissions — the same posture as a
//! Docker or SSH agent socket — which is why the permissions are set
//! explicitly rather than left to the umask.
//!
//! ## Portability
//!
//! Unix-domain sockets are the whole mechanism, so the server/client are
//! `#[cfg(unix)]`. Non-Unix targets get stubs that compile and fail loudly at
//! runtime, keeping `cargo check --target x86_64-pc-windows-gnu` green without
//! a second transport.

use serde::{Deserialize, Serialize};

use crate::control_plane::primary::DelegationStatusWire;

/// Environment override for the local socket path.
pub const SOCKET_ENV: &str = "OPENAB_AGENT_SOCKET";

/// Default socket path: `$OPENAB_AGENT_SOCKET` if set, else
/// `$HOME/.openab/agent.sock`. Returns an error only when neither the override
/// nor `HOME` is available — a headless environment with no home is a
/// misconfiguration the caller must resolve, not something to guess around.
pub fn default_socket_path() -> anyhow::Result<std::path::PathBuf> {
    if let Some(explicit) = std::env::var_os(SOCKET_ENV) {
        return Ok(std::path::PathBuf::from(explicit));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| anyhow::anyhow!("neither {SOCKET_ENV} nor HOME is set"))?;
    Ok(std::path::PathBuf::from(home)
        .join(".openab")
        .join("agent.sock"))
}

/// A spawn target as expressed by a local caller. Exactly one field must be
/// set; the server rejects a request that sets both or neither.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct LocalTarget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<std::collections::BTreeMap<String, String>>,
}

/// One request line on the local socket.
///
/// `#[serde(tag = "op")]` makes each variant a self-describing object, which is
/// what a CLI or MCP tool marshals by hand: `{"op":"spawn", ...}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LocalRequest {
    /// Initiate a delegation. Returns an opaque handle token once the CP acks
    /// routing. `deadline_secs` is relative to receipt; the server converts it
    /// to the absolute deadline the wire requires.
    Spawn {
        delegation_id: String,
        target: LocalTarget,
        prompt: String,
        deadline_secs: u64,
        /// Opaque parent handle token, if this spawn is a child of a
        /// delegation this runtime is serving.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
    },
    /// Block until the named delegation reaches a terminal state.
    Await { handle: String },
    /// Nonblocking status query: answers with the delegation's current state
    /// (pending / running / terminal) without waiting.
    Check { handle: String },
    /// Cancel an in-flight delegation by its opaque handle token.
    Cancel { handle: String, reason: String },
    /// Namespace-scoped roster snapshot.
    ListAgents,
}

/// A single agent as reported to a local caller. A projection of the wire
/// `AgentSummary`, kept separate so the local protocol does not re-export the
/// CP crate's types to a CLI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalAgent {
    pub name: String,
    pub agent_type: String,
    pub instance_id: String,
    pub labels: std::collections::BTreeMap<String, String>,
    pub active_sessions: u32,
    pub max_delegated_sessions: u32,
}

/// One response line on the local socket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LocalResponse {
    /// A spawn was admitted; `handle` is the opaque token to await/cancel with.
    Spawned { handle: String, assigned_to: String },
    /// A `check` found the delegation delegated but not yet acked by the CP.
    Pending,
    /// A `check` found the delegation admitted and being served by a peer.
    Running { assigned_to: String },
    /// A delegation reached a terminal state.
    Terminal {
        status: DelegationStatusWire,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A cancel was accepted by the CP.
    Cancelled,
    /// The roster snapshot.
    Agents { agents: Vec<LocalAgent> },
    /// The request could not be satisfied.
    Error { message: String },
}

impl LocalResponse {
    fn error(message: impl Into<String>) -> Self {
        LocalResponse::Error {
            message: message.into(),
        }
    }
}

#[cfg(unix)]
pub use unix_impl::{serve_local, LocalClient};

#[cfg(unix)]
mod unix_impl {
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use rand::RngCore;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::sync::watch;
    use tracing::{debug, info, warn};

    use crate::control_plane::primary::{
        ClientCommand, CommandError, DelegationHandle, DelegationSnapshot, SpawnRequest,
        SpawnTarget,
    };
    use crate::control_plane::ControlPlaneHandle;

    /// Maximum number of opaque handle tokens retained in the registry. Mirrors
    /// the CP's `max_inflight_delegations` default (ADR §Resource caps): a
    /// local surface can never track more live delegations than the hub admits,
    /// and a bound keeps a long-lived server from leaking tokens for
    /// delegations whose callers walked away.
    const MAX_HANDLE_TOKENS: usize = 4096;

    /// Number of random bytes behind each opaque token (256 bits, rendered as
    /// 64 lowercase hex chars). Large enough that a local caller cannot guess
    /// or fabricate a valid token for a delegation it did not spawn.
    const TOKEN_BYTES: usize = 32;

    /// Server-side map from opaque token → the real [`DelegationHandle`].
    ///
    /// The token handed to a local caller is an unstructured random string; it
    /// encodes nothing about the delegation (no `admission`, no id), so a
    /// caller cannot forge one or infer another delegation's handle from its
    /// own. The map is shared across every connection to this server (an
    /// `Arc<Mutex<_>>`), so a token minted on one connection resolves on
    /// another — the same delegation is reachable regardless of which
    /// connection awaits or cancels it.
    ///
    /// Bounded at [`MAX_HANDLE_TOKENS`] with deterministic oldest-first
    /// eviction: `order` records insertion order, and an insert over the cap
    /// evicts the front entry. Terminal delegations are not evicted eagerly —
    /// only age evicts — so a `check`/`await` after completion still resolves
    /// until the token ages out.
    #[derive(Default)]
    pub(super) struct HandleRegistry {
        by_token: HashMap<String, DelegationHandle>,
        order: VecDeque<String>,
    }

    impl HandleRegistry {
        /// Mint a fresh opaque token for `handle`, insert it, and evict the
        /// oldest token if the map is over capacity. Returns the token.
        fn insert(&mut self, handle: DelegationHandle) -> String {
            let token = mint_token();
            self.by_token.insert(token.clone(), handle);
            self.order.push_back(token.clone());
            while self.order.len() > MAX_HANDLE_TOKENS {
                if let Some(oldest) = self.order.pop_front() {
                    self.by_token.remove(&oldest);
                }
            }
            token
        }

        /// Resolve a token to its handle, or `None` if it was never minted here
        /// (a fabricated token) or has aged out.
        fn resolve(&self, token: &str) -> Option<DelegationHandle> {
            self.by_token.get(token).cloned()
        }

        #[cfg(test)]
        fn len(&self) -> usize {
            self.by_token.len()
        }
    }

    /// A shared, connection-independent handle registry.
    pub(super) type SharedRegistry = Arc<Mutex<HandleRegistry>>;

    /// Generate one opaque token: [`TOKEN_BYTES`] of CSPRNG output as hex.
    fn mint_token() -> String {
        let mut bytes = [0u8; TOKEN_BYTES];
        rand::thread_rng().fill_bytes(&mut bytes);
        let mut s = String::with_capacity(TOKEN_BYTES * 2);
        for b in bytes {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    /// Bind the local UDS server at `path`, serving requests by translating
    /// them into [`ClientCommand`]s on `cp`. Runs until `shutdown` flips.
    ///
    /// The socket and its parent directory are created owner-only. A stale
    /// socket from a prior run is removed first (a leftover file would make
    /// `bind` fail with `EADDRINUSE`).
    pub async fn serve_local(
        path: PathBuf,
        cp: ControlPlaneHandle,
        mut shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        prepare_socket_dir(&path)?;
        // Remove a stale socket file; ignore "not found".
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(anyhow::anyhow!("removing stale socket {path:?}: {e}")),
        }
        let listener = UnixListener::bind(&path)
            .map_err(|e| anyhow::anyhow!("binding local socket {path:?}: {e}"))?;
        // Owner-only on the socket node itself, not merely via the directory.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| anyhow::anyhow!("securing local socket {path:?}: {e}"))?;
        info!(socket = %path.display(), "control-plane local IPC server listening");

        // One registry for the whole server: tokens minted on any connection
        // resolve on any other.
        let registry: SharedRegistry = Arc::new(Mutex::new(HandleRegistry::default()));

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    info!("control-plane local IPC server shutting down");
                    break;
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _addr)) => {
                            let cp = cp.clone();
                            let registry = Arc::clone(&registry);
                            tokio::spawn(async move {
                                if let Err(e) = handle_conn(stream, cp, registry).await {
                                    debug!(error = %format!("{e:#}"), "local IPC connection ended with error");
                                }
                            });
                        }
                        Err(e) => {
                            warn!(error = %e, "local IPC accept failed");
                        }
                    }
                }
            }
        }
        // Best-effort cleanup so the next boot does not trip over the node.
        let _ = std::fs::remove_file(&path);
        Ok(())
    }

    /// Create the socket's parent directory owner-only if it does not exist,
    /// and tighten it to `0700` if it does. A world-writable parent would let
    /// another local user pre-create or replace the socket.
    fn prepare_socket_dir(path: &Path) -> anyhow::Result<()> {
        let Some(dir) = path.parent() else {
            return Ok(());
        };
        if dir.as_os_str().is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(dir)
            .map_err(|e| anyhow::anyhow!("creating socket dir {dir:?}: {e}"))?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| anyhow::anyhow!("securing socket dir {dir:?}: {e}"))?;
        Ok(())
    }

    /// One client connection: read request lines, answer each with one
    /// response line. A malformed line is answered with an `Error` rather than
    /// dropping the connection, so a CLI gets a diagnostic.
    async fn handle_conn(
        stream: UnixStream,
        cp: ControlPlaneHandle,
        registry: SharedRegistry,
    ) -> anyhow::Result<()> {
        let (read_half, mut write_half) = stream.into_split();
        let mut lines = BufReader::new(read_half).lines();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let response = match serde_json::from_str::<LocalRequest>(&line) {
                Ok(request) => dispatch(request, &cp, &registry).await,
                Err(e) => LocalResponse::error(format!("malformed request: {e}")),
            };
            let mut encoded = serde_json::to_string(&response)?;
            encoded.push('\n');
            write_half.write_all(encoded.as_bytes()).await?;
            write_half.flush().await?;
        }
        Ok(())
    }

    /// Minimum accepted `deadline_secs`. Zero (or a negative-by-underflow
    /// value) would spawn a delegation that is already past due, which the CP
    /// would sweep on arrival — a caller error worth rejecting locally.
    const MIN_DEADLINE_SECS: u64 = 1;
    /// Maximum accepted `deadline_secs`. Caps a delegation's absolute lifetime
    /// to the ADR's sensible ceiling (30 minutes); larger values are clamped so
    /// a caller cannot pin a slot open indefinitely.
    const MAX_DEADLINE_SECS: u64 = 1800;

    /// Translate one request into a command, submit it, and map the reply.
    async fn dispatch(
        request: LocalRequest,
        cp: &ControlPlaneHandle,
        registry: &SharedRegistry,
    ) -> LocalResponse {
        match request {
            LocalRequest::Spawn {
                delegation_id,
                target,
                prompt,
                deadline_secs,
                parent,
            } => {
                let target = match into_spawn_target(target) {
                    Ok(t) => t,
                    Err(e) => return LocalResponse::error(e),
                };
                // Reject a zero deadline outright; clamp an over-long one to the
                // ADR ceiling. Both are validated before any frame is sent.
                if deadline_secs < MIN_DEADLINE_SECS {
                    return LocalResponse::error(format!(
                        "deadline_secs must be at least {MIN_DEADLINE_SECS}"
                    ));
                }
                let deadline_secs = deadline_secs.min(MAX_DEADLINE_SECS);
                let parent = match parent {
                    Some(token) => match registry.lock().unwrap().resolve(&token) {
                        Some(h) => Some(h),
                        None => return LocalResponse::error("unknown parent handle token"),
                    },
                    None => None,
                };
                let deadline = chrono::Utc::now() + chrono::Duration::seconds(deadline_secs as i64);
                let request = SpawnRequest {
                    delegation_id,
                    target,
                    prompt,
                    deadline,
                    parent,
                };
                let (tx, rx) = tokio::sync::oneshot::channel();
                if cp
                    .submit(ClientCommand::Spawn { request, reply: tx })
                    .await
                    .is_err()
                {
                    return LocalResponse::error(CommandError::NotConnected.to_string());
                }
                match rx.await {
                    Ok(Ok(ack)) => {
                        // Mint an opaque token for the handle only now that the
                        // spawn succeeded, and report the CP-assigned peer.
                        let token = registry.lock().unwrap().insert(ack.handle);
                        LocalResponse::Spawned {
                            handle: token,
                            assigned_to: ack.assigned_to,
                        }
                    }
                    Ok(Err(e)) => LocalResponse::error(e.to_string()),
                    Err(_) => LocalResponse::error("control-plane client dropped the reply"),
                }
            }
            LocalRequest::Await { handle } => {
                let Some(handle) = registry.lock().unwrap().resolve(&handle) else {
                    return LocalResponse::error("unknown handle token");
                };
                let (tx, rx) = tokio::sync::oneshot::channel();
                if cp
                    .submit(ClientCommand::Await { handle, reply: tx })
                    .await
                    .is_err()
                {
                    return LocalResponse::error(CommandError::NotConnected.to_string());
                }
                match rx.await {
                    Ok(Ok(outcome)) => LocalResponse::Terminal {
                        status: outcome.status.into(),
                        result: outcome.result,
                        error: outcome.error,
                    },
                    Ok(Err(e)) => LocalResponse::error(e.to_string()),
                    Err(_) => LocalResponse::error("control-plane client dropped the reply"),
                }
            }
            LocalRequest::Check { handle } => {
                let Some(handle) = registry.lock().unwrap().resolve(&handle) else {
                    return LocalResponse::error("unknown handle token");
                };
                let (tx, rx) = tokio::sync::oneshot::channel();
                if cp
                    .submit(ClientCommand::Check { handle, reply: tx })
                    .await
                    .is_err()
                {
                    return LocalResponse::error(CommandError::NotConnected.to_string());
                }
                match rx.await {
                    Ok(Ok(DelegationSnapshot::Pending)) => LocalResponse::Pending,
                    Ok(Ok(DelegationSnapshot::Running { assigned_to })) => {
                        LocalResponse::Running { assigned_to }
                    }
                    Ok(Ok(DelegationSnapshot::Terminal(outcome))) => LocalResponse::Terminal {
                        status: outcome.status.into(),
                        result: outcome.result,
                        error: outcome.error,
                    },
                    Ok(Err(e)) => LocalResponse::error(e.to_string()),
                    Err(_) => LocalResponse::error("control-plane client dropped the reply"),
                }
            }
            LocalRequest::Cancel { handle, reason } => {
                let Some(handle) = registry.lock().unwrap().resolve(&handle) else {
                    return LocalResponse::error("unknown handle token");
                };
                let (tx, rx) = tokio::sync::oneshot::channel();
                if cp
                    .submit(ClientCommand::Cancel {
                        handle,
                        reason,
                        reply: tx,
                    })
                    .await
                    .is_err()
                {
                    return LocalResponse::error(CommandError::NotConnected.to_string());
                }
                match rx.await {
                    Ok(Ok(())) => LocalResponse::Cancelled,
                    Ok(Err(e)) => LocalResponse::error(e.to_string()),
                    Err(_) => LocalResponse::error("control-plane client dropped the reply"),
                }
            }
            LocalRequest::ListAgents => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if cp
                    .submit(ClientCommand::ListAgents { reply: tx })
                    .await
                    .is_err()
                {
                    return LocalResponse::error(CommandError::NotConnected.to_string());
                }
                match rx.await {
                    Ok(Ok(agents)) => LocalResponse::Agents {
                        agents: agents
                            .into_iter()
                            .map(|a| LocalAgent {
                                name: a.name,
                                agent_type: a.agent_type.to_string(),
                                instance_id: a.instance_id,
                                labels: a.labels,
                                active_sessions: a.active_sessions,
                                max_delegated_sessions: a.max_delegated_sessions,
                            })
                            .collect(),
                    },
                    Ok(Err(e)) => LocalResponse::error(e.to_string()),
                    Err(_) => LocalResponse::error("control-plane client dropped the reply"),
                }
            }
        }
    }

    fn into_spawn_target(target: LocalTarget) -> Result<SpawnTarget, String> {
        match (target.name, target.labels) {
            (Some(name), None) => {
                if name.is_empty() {
                    Err("target.name is empty".into())
                } else {
                    Ok(SpawnTarget::by_name(name))
                }
            }
            (None, Some(labels)) => {
                if labels.is_empty() {
                    Err("target.labels is empty".into())
                } else {
                    Ok(SpawnTarget::by_labels(labels))
                }
            }
            (Some(_), Some(_)) => {
                Err("target must set exactly one of name or labels, not both".into())
            }
            (None, None) => Err("target must set exactly one of name or labels".into()),
        }
    }

    /// A minimal client for the local socket, for a CLI or MCP tool linking
    /// this crate. Opens one connection per call: the protocol is
    /// request/response and the socket is cheap, so there is no pool.
    pub struct LocalClient {
        path: PathBuf,
    }

    impl LocalClient {
        /// Connect against an explicit path.
        pub fn new(path: impl Into<PathBuf>) -> Self {
            Self { path: path.into() }
        }

        /// Connect against [`default_socket_path`].
        pub fn from_default() -> anyhow::Result<Self> {
            Ok(Self {
                path: default_socket_path()?,
            })
        }

        /// Send one request and read one response.
        pub async fn request(&self, request: &LocalRequest) -> anyhow::Result<LocalResponse> {
            let stream = UnixStream::connect(&self.path)
                .await
                .map_err(|e| anyhow::anyhow!("connecting to local socket {:?}: {e}", self.path))?;
            let (read_half, mut write_half) = stream.into_split();
            let mut encoded = serde_json::to_string(request)?;
            encoded.push('\n');
            write_half.write_all(encoded.as_bytes()).await?;
            write_half.flush().await?;
            let mut lines = BufReader::new(read_half).lines();
            match lines.next_line().await? {
                Some(line) => Ok(serde_json::from_str(&line)?),
                None => Err(anyhow::anyhow!("local socket closed without a response")),
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use crate::control_plane::primary::{DelegationOutcome, SpawnAck};

        #[test]
        fn a_minted_token_is_opaque_hex_and_encodes_nothing_about_the_handle() {
            let mut reg = HandleRegistry::default();
            let handle = DelegationHandle::new("discord:12345:thread", 42);
            let token = reg.insert(handle.clone());
            // 32 random bytes → 64 lowercase hex chars.
            assert_eq!(token.len(), TOKEN_BYTES * 2);
            assert!(token
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
            // The token is not derived from the handle: it is neither the old
            // `admission:delegation_id` encoding nor does it embed the id
            // verbatim (a random hex string may incidentally contain a short
            // decimal like "42", so we test the structured forms, not digits).
            assert_ne!(
                token,
                format!("{}:{}", handle.admission(), handle.delegation_id())
            );
            assert_ne!(token, handle.delegation_id());
            assert!(!token.contains("discord"));
            assert!(!token.contains("thread"));
            assert!(!token.contains(':'));
            // It resolves back to the exact handle.
            assert_eq!(reg.resolve(&token), Some(handle));
        }

        #[test]
        fn two_tokens_for_the_same_handle_are_distinct_and_unguessable() {
            let mut reg = HandleRegistry::default();
            let handle = DelegationHandle::new("d-1", 7);
            let a = reg.insert(handle.clone());
            let b = reg.insert(handle);
            assert_ne!(a, b, "each mint draws fresh randomness");
        }

        #[test]
        fn a_fabricated_token_does_not_resolve() {
            let mut reg = HandleRegistry::default();
            let real = reg.insert(DelegationHandle::new("d-1", 1));
            // A plausible-looking but never-minted token is rejected.
            assert!(reg.resolve(&"0".repeat(TOKEN_BYTES * 2)).is_none());
            // The old fabricatable `admission:id` shape resolves to nothing.
            assert!(reg.resolve("1:d-1").is_none());
            assert!(reg.resolve("").is_none());
            // The genuine one still resolves.
            assert!(reg.resolve(&real).is_some());
        }

        #[test]
        fn the_registry_is_bounded_and_evicts_oldest_first() {
            let mut reg = HandleRegistry::default();
            // Fill to capacity, remembering the very first token.
            let first = reg.insert(DelegationHandle::new("d-0", 0));
            for i in 1..MAX_HANDLE_TOKENS {
                reg.insert(DelegationHandle::new(format!("d-{i}"), i as u64));
            }
            assert_eq!(reg.len(), MAX_HANDLE_TOKENS);
            assert!(reg.resolve(&first).is_some(), "still present at capacity");

            // One more insert evicts the oldest (the first) token, not a newer one.
            let newest = reg.insert(DelegationHandle::new("d-overflow", 9999));
            assert_eq!(reg.len(), MAX_HANDLE_TOKENS, "cap holds");
            assert!(reg.resolve(&first).is_none(), "oldest was evicted");
            assert!(reg.resolve(&newest).is_some(), "newest survives");
        }

        /// A stub CP loop that answers each command with a canned reply, so the
        /// full local dispatch path can be exercised without a socket to a hub.
        fn stub_cp() -> (ControlPlaneHandle, tokio::task::JoinHandle<()>) {
            let (handle, mut rx) = crate::control_plane::client::test_support::handle_and_rx();
            let task = tokio::spawn(async move {
                while let Some(command) = rx.recv().await {
                    match command {
                        ClientCommand::Spawn { request, reply } => {
                            let _ = reply.send(Ok(SpawnAck {
                                handle: DelegationHandle::new(request.delegation_id, 5),
                                assigned_to: "prod/worker-1".into(),
                            }));
                        }
                        ClientCommand::Check { reply, .. } => {
                            let _ = reply.send(Ok(DelegationSnapshot::Running {
                                assigned_to: "prod/worker-1".into(),
                            }));
                        }
                        ClientCommand::Await { reply, .. } => {
                            let _ = reply.send(Ok(DelegationOutcome {
                                status: openab_cp::proto::DelegationStatus::Completed,
                                result: Some("done".into()),
                                error: None,
                            }));
                        }
                        ClientCommand::Cancel { reply, .. } => {
                            let _ = reply.send(Ok(()));
                        }
                        ClientCommand::ListAgents { reply } => {
                            let _ = reply.send(Ok(vec![]));
                        }
                    }
                }
            });
            (handle, task)
        }

        #[tokio::test]
        async fn spawn_reports_the_assigned_peer_not_an_empty_string() {
            let (cp, task) = stub_cp();
            let registry: SharedRegistry = Arc::new(Mutex::new(HandleRegistry::default()));
            let resp = dispatch(
                LocalRequest::Spawn {
                    delegation_id: "d-1".into(),
                    target: LocalTarget {
                        name: Some("worker-1".into()),
                        labels: None,
                    },
                    prompt: "hi".into(),
                    deadline_secs: 60,
                    parent: None,
                },
                &cp,
                &registry,
            )
            .await;
            match resp {
                LocalResponse::Spawned {
                    handle,
                    assigned_to,
                } => {
                    assert_eq!(assigned_to, "prod/worker-1", "assigned_to must be reported");
                    // The handle is an opaque token, not `admission:id`.
                    assert_eq!(handle.len(), TOKEN_BYTES * 2);
                    assert!(!handle.contains(':'));
                    // And it resolves in the shared registry for a later check.
                    assert!(registry.lock().unwrap().resolve(&handle).is_some());
                }
                other => panic!("expected Spawned, got {other:?}"),
            }
            drop(cp);
            let _ = task.await;
        }

        #[tokio::test]
        async fn check_after_spawn_reports_running_without_blocking() {
            let (cp, task) = stub_cp();
            let registry: SharedRegistry = Arc::new(Mutex::new(HandleRegistry::default()));
            let spawned = dispatch(
                LocalRequest::Spawn {
                    delegation_id: "d-1".into(),
                    target: LocalTarget {
                        name: Some("worker-1".into()),
                        labels: None,
                    },
                    prompt: "hi".into(),
                    deadline_secs: 60,
                    parent: None,
                },
                &cp,
                &registry,
            )
            .await;
            let LocalResponse::Spawned { handle, .. } = spawned else {
                panic!("spawn failed");
            };
            let checked = dispatch(LocalRequest::Check { handle }, &cp, &registry).await;
            assert_eq!(
                checked,
                LocalResponse::Running {
                    assigned_to: "prod/worker-1".into()
                }
            );
            drop(cp);
            let _ = task.await;
        }

        #[tokio::test]
        async fn check_of_a_fabricated_token_is_rejected_before_reaching_the_cp() {
            let (cp, task) = stub_cp();
            let registry: SharedRegistry = Arc::new(Mutex::new(HandleRegistry::default()));
            let resp = dispatch(
                LocalRequest::Check {
                    handle: "deadbeef".into(),
                },
                &cp,
                &registry,
            )
            .await;
            assert!(matches!(resp, LocalResponse::Error { .. }));
            drop(cp);
            let _ = task.await;
        }

        #[tokio::test]
        async fn a_zero_deadline_is_rejected_and_never_reaches_the_cp() {
            let (cp, task) = stub_cp();
            let registry: SharedRegistry = Arc::new(Mutex::new(HandleRegistry::default()));
            let resp = dispatch(
                LocalRequest::Spawn {
                    delegation_id: "d-1".into(),
                    target: LocalTarget {
                        name: Some("worker-1".into()),
                        labels: None,
                    },
                    prompt: "hi".into(),
                    deadline_secs: 0,
                    parent: None,
                },
                &cp,
                &registry,
            )
            .await;
            match resp {
                LocalResponse::Error { message } => assert!(message.contains("deadline_secs")),
                other => panic!("expected an error, got {other:?}"),
            }
            // Nothing was tracked, since the spawn was rejected locally.
            assert_eq!(registry.lock().unwrap().len(), 0);
            drop(cp);
            let _ = task.await;
        }

        #[tokio::test]
        async fn an_over_long_deadline_is_clamped_to_the_adr_max() {
            let (cp, task) = stub_cp();
            let registry: SharedRegistry = Arc::new(Mutex::new(HandleRegistry::default()));
            // A value that, unclamped, would overflow `Duration::seconds(i64)`
            // and panic. The clamp to MAX_DEADLINE_SECS keeps it well-formed.
            let resp = dispatch(
                LocalRequest::Spawn {
                    delegation_id: "d-1".into(),
                    target: LocalTarget {
                        name: Some("worker-1".into()),
                        labels: None,
                    },
                    prompt: "hi".into(),
                    deadline_secs: u64::MAX,
                    parent: None,
                },
                &cp,
                &registry,
            )
            .await;
            // It succeeds (clamped, not rejected).
            assert!(matches!(resp, LocalResponse::Spawned { .. }));
            drop(cp);
            let _ = task.await;
        }

        use super::*;

        #[test]
        fn spawn_target_conversion_enforces_exactly_one() {
            assert!(into_spawn_target(LocalTarget {
                name: Some("w1".into()),
                labels: None
            })
            .is_ok());
            let mut labels = std::collections::BTreeMap::new();
            labels.insert("k".into(), "v".into());
            assert!(into_spawn_target(LocalTarget {
                name: None,
                labels: Some(labels.clone())
            })
            .is_ok());
            assert!(into_spawn_target(LocalTarget {
                name: Some("w1".into()),
                labels: Some(labels)
            })
            .is_err());
            assert!(into_spawn_target(LocalTarget::default()).is_err());
            assert!(into_spawn_target(LocalTarget {
                name: Some(String::new()),
                labels: None
            })
            .is_err());
        }

        #[tokio::test]
        async fn round_trips_a_request_and_response_over_a_real_socket() {
            // Bind a bare listener that echoes a canned response, exercising the
            // client's framing against a real UDS without a full CP.
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("agent.sock");
            let listener = UnixListener::bind(&path).unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (read_half, mut write_half) = stream.into_split();
                let mut lines = BufReader::new(read_half).lines();
                let line = lines.next_line().await.unwrap().unwrap();
                let req: LocalRequest = serde_json::from_str(&line).unwrap();
                assert!(matches!(req, LocalRequest::ListAgents));
                let resp = LocalResponse::Agents { agents: vec![] };
                let mut encoded = serde_json::to_string(&resp).unwrap();
                encoded.push('\n');
                write_half.write_all(encoded.as_bytes()).await.unwrap();
                write_half.flush().await.unwrap();
            });
            let client = LocalClient::new(path);
            let resp = client.request(&LocalRequest::ListAgents).await.unwrap();
            assert_eq!(resp, LocalResponse::Agents { agents: vec![] });
            server.await.unwrap();
        }

        #[tokio::test]
        async fn the_server_secures_the_socket_and_its_directory_owner_only() {
            use tokio::sync::watch;
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("nested").join("agent.sock");
            // A dummy CP handle: submissions will be rejected NotConnected, but
            // the test only checks the socket's permissions, not a round trip.
            let (client, _rx) = crate::control_plane::client::test_support::handle_and_rx();
            let (tx, rx) = watch::channel(false);
            let server_path = sock.clone();
            let server = tokio::spawn(async move { serve_local(server_path, client, rx).await });
            // Wait for the socket to appear.
            for _ in 0..100 {
                if sock.exists() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(sock.exists(), "server did not create the socket");
            let sock_mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
            assert_eq!(sock_mode, 0o600, "socket must be owner-only");
            let dir_mode = std::fs::metadata(sock.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700, "socket directory must be owner-only");
            let _ = tx.send(true);
            let _ = server.await;
        }
    }
}

// ---------------------------------------------------------------------------
// Non-Unix stubs: compile everywhere, fail loudly at runtime.
// ---------------------------------------------------------------------------

#[cfg(not(unix))]
pub use non_unix_impl::{serve_local, LocalClient};

#[cfg(not(unix))]
mod non_unix_impl {
    use super::*;
    use std::path::PathBuf;

    use tokio::sync::watch;

    use crate::control_plane::ControlPlaneHandle;

    /// Not supported off Unix: there is no unix-domain socket transport. The
    /// symbol exists so the crate compiles for `x86_64-pc-windows-gnu`.
    pub async fn serve_local(
        _path: PathBuf,
        _cp: ControlPlaneHandle,
        _shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        Err(anyhow::anyhow!(
            "the control-plane local IPC socket is only supported on Unix"
        ))
    }

    /// Non-Unix stub client.
    pub struct LocalClient {
        _path: PathBuf,
    }

    impl LocalClient {
        pub fn new(path: impl Into<PathBuf>) -> Self {
            Self { _path: path.into() }
        }

        pub fn from_default() -> anyhow::Result<Self> {
            Ok(Self {
                _path: default_socket_path()?,
            })
        }

        pub async fn request(&self, _request: &LocalRequest) -> anyhow::Result<LocalResponse> {
            Err(anyhow::anyhow!(
                "the control-plane local IPC socket is only supported on Unix"
            ))
        }
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;

    #[test]
    fn default_socket_path_prefers_env_then_falls_back_to_home() {
        // Env mutation is process-global; keep both cases in one test so they
        // do not race a sibling test that also touches these vars, and restore
        // on exit.
        let prev_sock = std::env::var_os(SOCKET_ENV);
        let prev_home = std::env::var_os("HOME");

        std::env::set_var(SOCKET_ENV, "/tmp/custom-agent.sock");
        assert_eq!(
            default_socket_path().unwrap(),
            std::path::PathBuf::from("/tmp/custom-agent.sock")
        );

        std::env::remove_var(SOCKET_ENV);
        std::env::set_var("HOME", "/home/tester");
        assert_eq!(
            default_socket_path().unwrap(),
            std::path::PathBuf::from("/home/tester/.openab/agent.sock")
        );

        match prev_sock {
            Some(v) => std::env::set_var(SOCKET_ENV, v),
            None => std::env::remove_var(SOCKET_ENV),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn requests_are_tagged_by_op() {
        let spawn = LocalRequest::Spawn {
            delegation_id: "d-1".into(),
            target: LocalTarget {
                name: Some("w1".into()),
                labels: None,
            },
            prompt: "hi".into(),
            deadline_secs: 60,
            parent: None,
        };
        let v = serde_json::to_value(&spawn).unwrap();
        assert_eq!(v["op"], "spawn");
        assert_eq!(v["deadline_secs"], 60);
        // Round-trips.
        let back: LocalRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back, spawn);

        let list = serde_json::to_value(LocalRequest::ListAgents).unwrap();
        assert_eq!(list["op"], "list_agents");

        let check = serde_json::to_value(LocalRequest::Check {
            handle: "tok".into(),
        })
        .unwrap();
        assert_eq!(check["op"], "check");
        assert_eq!(check["handle"], "tok");
    }

    #[test]
    fn responses_are_tagged_by_kind() {
        let spawned = serde_json::to_value(LocalResponse::Spawned {
            handle: "a1b2c3".into(),
            assigned_to: "prod/w1".into(),
        })
        .unwrap();
        assert_eq!(spawned["kind"], "spawned");
        assert_eq!(spawned["handle"], "a1b2c3");
        assert_eq!(spawned["assigned_to"], "prod/w1");

        // The Check-family variants are each self-describing.
        let pending = serde_json::to_value(LocalResponse::Pending).unwrap();
        assert_eq!(pending["kind"], "pending");
        let running = serde_json::to_value(LocalResponse::Running {
            assigned_to: "prod/w1".into(),
        })
        .unwrap();
        assert_eq!(running["kind"], "running");
        assert_eq!(running["assigned_to"], "prod/w1");

        // The Terminal variant keeps its own `status` field alongside the tag.
        let terminal = serde_json::to_value(LocalResponse::Terminal {
            status: DelegationStatusWire::Completed,
            result: Some("done".into()),
            error: None,
        })
        .unwrap();
        assert_eq!(terminal["kind"], "terminal");
        assert_eq!(terminal["status"], "completed");

        let err = serde_json::to_value(LocalResponse::error("nope")).unwrap();
        assert_eq!(err["kind"], "error");
        assert_eq!(err["message"], "nope");
    }
}

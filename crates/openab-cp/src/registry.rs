//! Instance registry: who is alive, with which claims, on which connection.
//!
//! Replica semantics (ADR §3): multiple instances may register under one
//! logical identity during rolling deploys. New delegations route to the
//! newest healthy instance; in-flight delegations complete on the instance
//! that accepted them. Lease expiry (missed heartbeats) deregisters an
//! instance and fails its in-flight delegations.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tokio::sync::{mpsc, watch};

use crate::proto::AgentType;

/// Capacity of each per-connection outbound queue, in entries. Entries alone
/// are not a memory bound — see [`OutboundBudget`], which bounds the bytes.
pub const OUTBOUND_QUEUE: usize = 256;

/// Byte budget shared by one connection's [`FrameTx`] clones and its
/// [`FrameRx`].
///
/// The entry-bounded queue does not bound memory: frame sizes are
/// independently configurable, so 256 queued frames is entry-legal and
/// arbitrarily large in bytes (a stalled initiator with an 8 MiB result budget
/// could hold ~2 GiB). Bytes are reserved before an enqueue and released on
/// dequeue or teardown, so a stalled peer is refused on whichever bound it hits
/// first.
#[derive(Debug)]
pub struct OutboundBudget {
    reserved: AtomicUsize,
    max: usize,
}

impl OutboundBudget {
    fn new(max: usize) -> Self {
        Self {
            reserved: AtomicUsize::new(0),
            max,
        }
    }

    /// Reserve `n` bytes, or refuse. Refusal is not an error state: the caller
    /// treats it exactly like a full queue (the peer is disconnected).
    fn reserve(&self, n: usize) -> bool {
        self.reserved
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
                match cur.checked_add(n) {
                    Some(next) if next <= self.max => Some(next),
                    _ => None,
                }
            })
            .is_ok()
    }

    fn release(&self, n: usize) {
        self.reserved.fetch_sub(n, Ordering::AcqRel);
    }

    /// Bytes currently reserved (queued but not yet dequeued).
    pub fn reserved_bytes(&self) -> usize {
        self.reserved.load(Ordering::Acquire)
    }

    pub fn max_bytes(&self) -> usize {
        self.max
    }
}

/// Why an outbound frame was refused. Both outcomes mean the same thing to
/// callers — this peer cannot take the frame, so treat it as disconnected —
/// and are distinguished only for logs and tests.
#[derive(Debug, PartialEq, Eq)]
pub enum SendRefused {
    /// The connection is gone (receiver dropped).
    Closed,
    /// The queue is full, in entries or in bytes.
    Full,
}

/// Outbound frame sender for one WS connection (serialized JSON text).
///
/// Bounded twice — in entries by the channel and in bytes by
/// [`OutboundBudget`] — because a peer that cannot drain its queue must be
/// disconnected rather than grow CP memory. Cloneable: the registry hands
/// clones to every path that routes a frame to this connection.
#[derive(Clone, Debug)]
pub struct FrameTx {
    tx: mpsc::Sender<String>,
    budget: Arc<OutboundBudget>,
}

impl FrameTx {
    /// Enqueue one frame without blocking. Bytes are reserved BEFORE the
    /// enqueue (and released again if the enqueue fails), so the budget can
    /// never be observed lower than what the queue actually holds.
    pub fn try_send(&self, text: String) -> Result<(), SendRefused> {
        let n = text.len();
        if !self.budget.reserve(n) {
            return Err(SendRefused::Full);
        }
        match self.tx.try_send(text) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.budget.release(n);
                Err(SendRefused::Full)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.budget.release(n);
                Err(SendRefused::Closed)
            }
        }
    }

    /// The connection's byte budget (for observability and tests).
    pub fn budget(&self) -> &Arc<OutboundBudget> {
        &self.budget
    }
}

/// Receiving half of one connection's outbound queue, owned by that
/// connection's task. Dequeueing releases the frame's byte reservation;
/// dropping it (connection teardown) releases whatever is still queued, so a
/// torn-down connection never leaves reservations behind.
pub struct FrameRx {
    rx: mpsc::Receiver<String>,
    budget: Arc<OutboundBudget>,
}

impl FrameRx {
    pub async fn recv(&mut self) -> Option<String> {
        let text = self.rx.recv().await?;
        self.budget.release(text.len());
        Some(text)
    }

    /// Non-blocking dequeue (tests and drain loops).
    pub fn try_recv(&mut self) -> Result<String, mpsc::error::TryRecvError> {
        let text = self.rx.try_recv()?;
        self.budget.release(text.len());
        Ok(text)
    }

    /// Stop accepting frames (tests simulate a dead peer with this).
    pub fn close(&mut self) {
        self.rx.close();
    }
}

impl Drop for FrameRx {
    fn drop(&mut self) {
        // Teardown release: drain what is still queued so any surviving
        // sender clone sees the budget freed rather than permanently consumed
        // by a connection that no longer exists.
        self.rx.close();
        while let Ok(text) = self.rx.try_recv() {
            self.budget.release(text.len());
        }
    }
}

/// Create one connection's outbound queue: bounded in entries by
/// [`OUTBOUND_QUEUE`] and in bytes by `max_bytes`.
pub fn outbound_channel(max_bytes: usize) -> (FrameTx, FrameRx) {
    let (tx, rx) = mpsc::channel(OUTBOUND_QUEUE);
    let budget = Arc::new(OutboundBudget::new(max_bytes));
    (
        FrameTx {
            tx,
            budget: Arc::clone(&budget),
        },
        FrameRx { rx, budget },
    )
}

/// Shutdown signal for one WS connection, held by the registry so the CP can
/// terminate a connection it no longer considers registered. Lease expiry must
/// close the socket — otherwise the connection task lives on with a registry
/// entry that no longer exists, and the client cannot re-register because
/// registration is first-frame-only.
///
/// `watch` (not `oneshot`) so the connection task can select on it repeatedly,
/// and wrapped in `Arc` so the registry entry and the connection task share
/// one signal without either side's drop cancelling it.
pub type ShutdownTx = Arc<watch::Sender<Option<&'static str>>>;

/// Create a fresh connection shutdown signal. The connection task keeps the
/// returned handle (to `subscribe()`), the registry keeps a clone. The value
/// is `None` until the CP requests the close, then the close reason (sent on
/// the WS close frame so the client knows why it was terminated).
pub fn shutdown_signal() -> ShutdownTx {
    Arc::new(watch::channel(None).0)
}

/// Registry slot: the public instance view plus CP-internal connection
/// control that is deliberately not part of `Instance` (nothing routed or
/// serialized should carry it).
struct Entry {
    inst: Instance,
    shutdown: ShutdownTx,
}

/// A live, authenticated, registered runtime instance.
#[derive(Clone, Debug)]
pub struct Instance {
    /// CP-generated registration handle — the registry key and the basis of
    /// all ownership checks. Never client-supplied: a colliding client
    /// `instance_id` cannot replace or tear down another identity's
    /// registration.
    pub handle: u64,
    pub namespace: String,
    pub name: String,
    pub agent_type: AgentType,
    /// Client-supplied replica discriminator (display/audit only; ownership
    /// and teardown key on `handle`).
    pub instance_id: String,
    pub labels: BTreeMap<String, String>,
    pub max_delegated_sessions: u32,
    /// Delegations currently routed to this instance. CP-owned and
    /// authoritative — never merged from runtime reports.
    pub active_sessions: u32,
    pub registered_at: Instant,
    pub last_heartbeat: Instant,
    pub tx: FrameTx,
}

impl Instance {
    pub fn logical_id(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }

    pub fn saturated(&self) -> bool {
        self.active_sessions >= self.max_delegated_sessions
    }

    fn matches_labels(&self, want: &BTreeMap<String, String>) -> bool {
        want.iter()
            .all(|(k, v)| self.labels.get(k).map(|x| x == v).unwrap_or(false))
    }
}

#[derive(Default)]
pub struct Registry {
    /// Keyed by CP-generated registration handle.
    inner: RwLock<BTreeMap<u64, Entry>>,
    next_handle: AtomicU64,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a newly registered instance under a fresh CP-generated handle
    /// (returned). Re-registrations (reconnects) get a new handle; the stale
    /// entry disappears when its socket closes or its lease expires — it can
    /// never be replaced by another connection's registration.
    ///
    /// `shutdown` is the owning connection's termination signal: the CP
    /// triggers it whenever it drops the registration on its own initiative
    /// (lease expiry).
    pub fn register_conn(&self, mut inst: Instance, shutdown: ShutdownTx) -> u64 {
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed) + 1;
        inst.handle = handle;
        self.inner.write().insert(handle, Entry { inst, shutdown });
        handle
    }

    /// Register an instance with a detached shutdown signal (no connection
    /// task is listening). For tests and non-WS callers.
    pub fn register(&self, inst: Instance) -> u64 {
        self.register_conn(inst, shutdown_signal())
    }

    /// Ask the owning connection task to close, with the reason to put on
    /// the close frame. Returns whether a live registration was signalled.
    /// Must be called BEFORE `deregister`, which drops the registry's handle
    /// on the signal.
    pub fn signal_shutdown(&self, handle: u64, reason: &'static str) -> bool {
        match self.inner.read().get(&handle) {
            Some(e) => {
                // `send_replace` cannot fail even with no receivers left.
                e.shutdown.send_replace(Some(reason));
                true
            }
            None => false,
        }
    }

    /// Remove an instance by its registration handle (disconnect or lease
    /// expiry). Only the owning connection or the sweeper knows the handle.
    pub fn deregister(&self, handle: u64) -> Option<Instance> {
        self.inner.write().remove(&handle).map(|e| e.inst)
    }

    /// Refresh the lease. The runtime-reported session count is intentionally
    /// ignored: CP-owned in-flight accounting is authoritative, and merging
    /// runtime reports could pin an instance saturated forever.
    pub fn heartbeat(&self, handle: u64) -> bool {
        let mut g = self.inner.write();
        match g.get_mut(&handle) {
            Some(e) => {
                e.inst.last_heartbeat = Instant::now();
                true
            }
            None => false,
        }
    }

    /// Handles whose lease has expired.
    pub fn expired(&self, lease: Duration) -> Vec<u64> {
        let now = Instant::now();
        self.inner
            .read()
            .values()
            .filter(|e| now.duration_since(e.inst.last_heartbeat) > lease)
            .map(|e| e.inst.handle)
            .collect()
    }

    pub fn get(&self, handle: u64) -> Option<Instance> {
        self.inner.read().get(&handle).map(|e| e.inst.clone())
    }

    /// Select a serving instance within `namespace` by exact name or labels.
    ///
    /// Unsaturated matches only. Ordering:
    /// - exact-name selection → replicas of one logical agent: newest
    ///   registration first (rolling-deploy rule), load as tie-breaker
    /// - label selection → across logical agents: least loaded first,
    ///   registration recency as tie-breaker
    pub fn select(
        &self,
        namespace: &str,
        name: Option<&str>,
        labels: Option<&BTreeMap<String, String>>,
    ) -> Result<Instance, SelectError> {
        let g = self.inner.read();
        let mut matches: Vec<&Instance> = g
            .values()
            .map(|e| &e.inst)
            .filter(|i| i.namespace == namespace)
            .filter(|i| match name {
                Some(n) => i.name == n,
                None => true,
            })
            .filter(|i| match labels {
                Some(want) => i.matches_labels(want),
                None => true,
            })
            .collect();

        if matches.is_empty() {
            return Err(SelectError::NoTarget);
        }
        matches.retain(|i| !i.saturated());
        if matches.is_empty() {
            return Err(SelectError::Saturated);
        }
        if name.is_some() {
            matches.sort_by(|a, b| {
                b.registered_at
                    .cmp(&a.registered_at)
                    .then(a.active_sessions.cmp(&b.active_sessions))
            });
        } else {
            matches.sort_by(|a, b| {
                a.active_sessions
                    .cmp(&b.active_sessions)
                    .then(b.registered_at.cmp(&a.registered_at))
            });
        }
        Ok(matches[0].clone())
    }

    /// Adjust the CP-owned in-flight count for an instance.
    pub fn adjust_sessions(&self, handle: u64, delta: i32) {
        let mut g = self.inner.write();
        if let Some(e) = g.get_mut(&handle) {
            e.inst.active_sessions = e.inst.active_sessions.saturating_add_signed(delta);
        }
    }

    /// Registry snapshot for one namespace (basis for a future `list_agents`).
    pub fn list(&self, namespace: &str) -> Vec<Instance> {
        self.inner
            .read()
            .values()
            .map(|e| &e.inst)
            .filter(|i| i.namespace == namespace)
            .cloned()
            .collect()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SelectError {
    NoTarget,
    Saturated,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(ns: &str, name: &str, id: &str, max: u32) -> Instance {
        let (tx, _rx) = outbound_channel(1024 * 1024);
        Instance {
            handle: 0, // assigned by register()
            namespace: ns.into(),
            name: name.into(),
            agent_type: AgentType::Worker,
            instance_id: id.into(),
            labels: BTreeMap::new(),
            max_delegated_sessions: max,
            active_sessions: 0,
            registered_at: Instant::now(),
            last_heartbeat: Instant::now(),
            tx,
        }
    }

    #[test]
    fn outbound_queue_is_bounded_in_bytes_not_only_entries() {
        // 256 entries is not a memory bound: with independently configurable
        // frame sizes a stalled peer can hold entries × max-frame bytes. The
        // byte budget must refuse well before the entry count is exhausted.
        const BUDGET: usize = 64 * 1024;
        const FRAME: usize = 8 * 1024;
        let (tx, rx) = outbound_channel(BUDGET);

        let mut accepted = 0;
        loop {
            match tx.try_send("x".repeat(FRAME)) {
                Ok(()) => accepted += 1,
                Err(e) => {
                    assert_eq!(e, SendRefused::Full);
                    break;
                }
            }
            assert!(accepted <= OUTBOUND_QUEUE, "entry bound would have won");
        }
        assert_eq!(
            accepted,
            BUDGET / FRAME,
            "the byte budget, not the entry count, must be the binding limit"
        );
        assert!(
            accepted < OUTBOUND_QUEUE,
            "refusal must happen well before entry-count exhaustion ({accepted} < {OUTBOUND_QUEUE})"
        );
        assert_eq!(tx.budget().reserved_bytes(), BUDGET);

        // A frame that cannot fit is refused without consuming budget, and a
        // smaller one that does fit is still accepted (no sticky refusal).
        drop(rx);
        let (tx2, mut rx2) = outbound_channel(BUDGET);
        assert!(tx2.try_send("y".repeat(BUDGET + 1)).is_err());
        assert_eq!(
            tx2.budget().reserved_bytes(),
            0,
            "a refused reservation must not leak budget"
        );
        assert!(tx2.try_send("y".repeat(FRAME)).is_ok());
        assert_eq!(tx2.budget().reserved_bytes(), FRAME);

        // Dequeueing releases the reservation.
        assert_eq!(rx2.try_recv().unwrap().len(), FRAME);
        assert_eq!(tx2.budget().reserved_bytes(), 0);
    }

    #[test]
    fn teardown_releases_the_whole_outbound_reservation() {
        // Dropping the receiving half is connection teardown: whatever is
        // still queued must be released, or a surviving sender clone would see
        // the budget permanently consumed by a connection that is gone.
        const BUDGET: usize = 32 * 1024;
        let (tx, rx) = outbound_channel(BUDGET);
        let observer = Arc::clone(tx.budget());
        for _ in 0..4 {
            tx.try_send("z".repeat(4 * 1024)).unwrap();
        }
        assert_eq!(observer.reserved_bytes(), 16 * 1024);

        drop(rx);
        assert_eq!(
            observer.reserved_bytes(),
            0,
            "teardown must release every queued frame's reservation"
        );
        // The connection is gone, so further sends are refused as closed —
        // and still leak no budget.
        assert_eq!(tx.try_send("more".into()), Err(SendRefused::Closed));
        assert_eq!(observer.reserved_bytes(), 0);
    }

    #[test]
    fn select_by_name_and_namespace_isolation() {
        let r = Registry::new();
        let h1 = r.register(inst("prod", "w1", "i-1", 2));
        r.register(inst("dev", "w1", "i-2", 2));
        let got = r.select("prod", Some("w1"), None).unwrap();
        assert_eq!(got.handle, h1);
        assert!(matches!(
            r.select("staging", Some("w1"), None),
            Err(SelectError::NoTarget)
        ));
    }

    #[test]
    fn replicas_route_to_newest() {
        let r = Registry::new();
        r.register(inst("prod", "w1", "i-old", 2));
        std::thread::sleep(Duration::from_millis(5));
        let h_new = r.register(inst("prod", "w1", "i-new", 2));
        let got = r.select("prod", Some("w1"), None).unwrap();
        assert_eq!(got.handle, h_new);
    }

    #[test]
    fn saturation_is_distinct_from_no_target() {
        let r = Registry::new();
        let mut i = inst("prod", "w1", "i-1", 1);
        i.active_sessions = 1;
        r.register(i);
        assert!(matches!(
            r.select("prod", Some("w1"), None),
            Err(SelectError::Saturated)
        ));
        assert!(matches!(
            r.select("prod", Some("nope"), None),
            Err(SelectError::NoTarget)
        ));
    }

    #[test]
    fn label_selection_least_loaded_first() {
        let r = Registry::new();
        // Older but less loaded instance must win under label selection
        // (load first, recency only as tie-breaker — the inverse of the
        // exact-name replica rule).
        let mut a = inst("prod", "wa", "i-a", 4);
        a.labels.insert("backend".into(), "kiro".into());
        a.active_sessions = 0;
        let h_a = r.register(a);
        std::thread::sleep(Duration::from_millis(5));
        let mut b = inst("prod", "wb", "i-b", 4);
        b.labels.insert("backend".into(), "kiro".into());
        b.active_sessions = 3;
        r.register(b);

        let mut want = BTreeMap::new();
        want.insert("backend".to_string(), "kiro".to_string());
        let got = r.select("prod", None, Some(&want)).unwrap();
        assert_eq!(got.handle, h_a, "least loaded wins despite being older");

        // partial label mismatch -> NoTarget
        want.insert("arch".to_string(), "x86".to_string());
        assert!(matches!(
            r.select("prod", None, Some(&want)),
            Err(SelectError::NoTarget)
        ));
    }

    #[test]
    fn name_selection_newest_first_even_if_more_loaded() {
        let r = Registry::new();
        let mut old = inst("prod", "w1", "i-old", 4);
        old.active_sessions = 0;
        r.register(old);
        std::thread::sleep(Duration::from_millis(5));
        let mut new = inst("prod", "w1", "i-new", 4);
        new.active_sessions = 2;
        let h_new = r.register(new);
        let got = r.select("prod", Some("w1"), None).unwrap();
        assert_eq!(got.handle, h_new, "replica rule: newest registration wins");
    }

    #[test]
    fn lease_expiry_and_heartbeat() {
        let r = Registry::new();
        let h = r.register(inst("prod", "w1", "i-1", 1));
        assert!(r.expired(Duration::from_secs(60)).is_empty());
        assert!(r.heartbeat(h));
        assert!(!r.heartbeat(h + 999));
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(r.expired(Duration::ZERO), vec![h]);
    }

    #[test]
    fn colliding_instance_id_cannot_replace_other_registration() {
        // A second connection registering the same client-supplied
        // instance_id gets its own handle; the first registration survives
        // and can only be torn down via its own handle.
        let r = Registry::new();
        let h1 = r.register(inst("prod", "w1", "i-same", 1));
        let h2 = r.register(inst("prod", "w2", "i-same", 1));
        assert_ne!(h1, h2);
        assert_eq!(r.list("prod").len(), 2);
        // Tearing down the second leaves the first intact.
        assert!(r.deregister(h2).is_some());
        assert!(r.get(h1).is_some());
        // Deregistering an already-gone handle is a no-op.
        assert!(r.deregister(h2).is_none());
    }

    #[test]
    fn heartbeat_does_not_mutate_session_count() {
        let r = Registry::new();
        let h = r.register(inst("prod", "w1", "i-1", 2));
        r.adjust_sessions(h, 1);
        assert!(r.heartbeat(h));
        assert_eq!(
            r.get(h).unwrap().active_sessions,
            1,
            "CP-owned count is authoritative; heartbeat never changes it"
        );
    }

    #[test]
    fn adjust_sessions_saturating() {
        let r = Registry::new();
        let h = r.register(inst("prod", "w1", "i-1", 2));
        r.adjust_sessions(h, 1);
        assert_eq!(r.get(h).unwrap().active_sessions, 1);
        r.adjust_sessions(h, -5);
        assert_eq!(r.get(h).unwrap().active_sessions, 0);
    }

    #[tokio::test]
    async fn signal_shutdown_reaches_the_owning_connection() {
        // The CP must be able to terminate a connection
        // whose registration it drops on its own initiative.
        let r = Registry::new();
        let sig = shutdown_signal();
        let mut rx = sig.subscribe();
        let h = r.register_conn(inst("prod", "w1", "i-1", 1), sig);
        assert!(rx.borrow().is_none());

        assert!(r.signal_shutdown(h, "lease expired"));
        rx.changed().await.unwrap();
        assert_eq!(
            *rx.borrow(),
            Some("lease expired"),
            "connection task must observe the signal and its reason"
        );

        // After deregistration there is nothing left to signal.
        r.deregister(h);
        assert!(!r.signal_shutdown(h, "lease expired"));
    }

    #[tokio::test]
    async fn signal_shutdown_survives_registry_drop_of_the_entry() {
        // The connection task keeps its own handle on the signal, so a
        // signal delivered before deregistration is never lost.
        let r = Registry::new();
        let sig = shutdown_signal();
        let mut rx = sig.subscribe();
        let h = r.register_conn(inst("prod", "w1", "i-1", 1), sig);
        r.signal_shutdown(h, "lease expired");
        r.deregister(h);
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow(), Some("lease expired"));
    }
}

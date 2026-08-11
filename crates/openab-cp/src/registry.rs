//! Instance registry: who is alive, with which claims, on which connection.
//!
//! Replica semantics (ADR §3): multiple instances may register under one
//! logical identity during rolling deploys. New delegations route to the
//! newest healthy instance; in-flight delegations complete on the instance
//! that accepted them. Lease expiry (missed heartbeats) deregisters an
//! instance and fails its in-flight delegations.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tokio::sync::mpsc;

use crate::proto::AgentType;

/// Outbound frame sender for one WS connection (serialized JSON text).
/// Bounded: a peer that cannot drain its queue is disconnected rather than
/// growing CP memory (review F5).
pub type FrameTx = mpsc::Sender<String>;

/// Capacity of each per-connection outbound queue.
pub const OUTBOUND_QUEUE: usize = 256;

/// A live, authenticated, registered runtime instance.
#[derive(Clone, Debug)]
pub struct Instance {
    /// CP-generated registration handle — the registry key and the basis of
    /// all ownership checks. Never client-supplied (review F1): a colliding
    /// client `instance_id` cannot replace or tear down another identity's
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
    /// authoritative — never merged from runtime reports (review F6).
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
    inner: RwLock<BTreeMap<u64, Instance>>,
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
    pub fn register(&self, mut inst: Instance) -> u64 {
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed) + 1;
        inst.handle = handle;
        self.inner.write().insert(handle, inst);
        handle
    }

    /// Remove an instance by its registration handle (disconnect or lease
    /// expiry). Only the owning connection or the sweeper knows the handle.
    pub fn deregister(&self, handle: u64) -> Option<Instance> {
        self.inner.write().remove(&handle)
    }

    /// Refresh the lease. The runtime-reported session count is intentionally
    /// ignored: CP-owned in-flight accounting is authoritative (review F6 —
    /// merging reports could pin an instance saturated forever).
    pub fn heartbeat(&self, handle: u64) -> bool {
        let mut g = self.inner.write();
        match g.get_mut(&handle) {
            Some(i) => {
                i.last_heartbeat = Instant::now();
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
            .filter(|i| now.duration_since(i.last_heartbeat) > lease)
            .map(|i| i.handle)
            .collect()
    }

    pub fn get(&self, handle: u64) -> Option<Instance> {
        self.inner.read().get(&handle).cloned()
    }

    /// Select a serving instance within `namespace` by exact name or labels.
    ///
    /// Unsaturated matches only. Ordering (review F6):
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
        if let Some(i) = g.get_mut(&handle) {
            i.active_sessions = i.active_sessions.saturating_add_signed(delta);
        }
    }

    /// Registry snapshot for one namespace (basis for a future `list_agents`).
    pub fn list(&self, namespace: &str) -> Vec<Instance> {
        self.inner
            .read()
            .values()
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
        let (tx, _rx) = mpsc::channel(OUTBOUND_QUEUE);
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
        // (inverse recency/load — review F6).
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
        // Review F1: a second connection registering the same client-supplied
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
}

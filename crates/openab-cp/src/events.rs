//! Observer event fan-out — the "lobby" surface.
//!
//! Design invariants:
//!
//! - **Per-namespace sequence numbers.** Each namespace has its own monotonic
//!   counter, so an observer sees a dense stream and a gap unambiguously
//!   means "frames were dropped / the CP restarted — resync via
//!   `cp/list_agents`". A process-global counter would manufacture gaps out
//!   of unrelated activity in other namespaces.
//! - **Best effort, never blocking.** Fan-out uses `try_send` on the same
//!   bounded per-connection queue everything else uses: an observer that
//!   cannot keep up loses frames (and detects it via `seq`). The delegation
//!   path is never awaited, slowed, or failed because of a lobby client.
//! - **Namespace isolation.** An event is only ever offered to observers
//!   registered in that same namespace.
//! - **Bounded bodies.** Prompt/result excerpts are truncated with the same
//!   marker-inside-the-cap helper the router uses for oversized results, and
//!   are omitted entirely for `metadata_only` namespaces.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::config::CpConfig;
use crate::proto::{methods, CpEvent, EventParams, JsonRpcNotification};
use crate::registry::Registry;
use crate::router::truncate_with_marker;

/// Serializes one `cp/event` notification per emission and offers it to every
/// observer in the target namespace.
#[derive(Debug)]
pub struct EventHub {
    /// namespace → its ordered event stream. The outer lock only guards
    /// map get-or-create; ordering is enforced by the inner per-namespace
    /// lock (see [`EventHub::emit`]).
    streams: Mutex<BTreeMap<String, Arc<Mutex<u64>>>>,
    max_excerpt_bytes: usize,
    /// Namespaces configured `metadata_only = true`.
    metadata_only: BTreeSet<String>,
}

impl EventHub {
    pub fn new(cfg: &CpConfig) -> Self {
        Self {
            streams: Mutex::new(BTreeMap::new()),
            max_excerpt_bytes: cfg.max_event_excerpt_bytes,
            metadata_only: cfg
                .namespaces
                .iter()
                .filter(|(_, p)| p.metadata_only)
                .map(|(ns, _)| ns.clone())
                .collect(),
        }
    }

    /// Whether payload bodies are withheld from this namespace's events.
    pub fn is_metadata_only(&self, namespace: &str) -> bool {
        self.metadata_only.contains(namespace)
    }

    /// Bounded excerpt of an **agent-supplied** body (prompt, result, or a
    /// runtime-reported error). `None` when the namespace is `metadata_only`.
    pub fn excerpt(&self, namespace: &str, body: &str) -> Option<String> {
        if self.is_metadata_only(namespace) {
            return None;
        }
        Some(truncate_with_marker(body, self.max_excerpt_bytes))
    }

    /// [`EventHub::excerpt`] over an optional body.
    pub fn excerpt_opt(&self, namespace: &str, body: Option<&str>) -> Option<String> {
        body.and_then(|b| self.excerpt(namespace, b))
    }

    /// Bound a short **CP-synthesized** diagnostic (a timeout or disconnect
    /// reason the CP itself composed). These are metadata, not payload, so
    /// `metadata_only` does not suppress them.
    ///
    /// NEVER pass agent-supplied content here: anything that originated in a
    /// client frame (prompts, results, errors, an initiator's cancel reason)
    /// must go through [`EventHub::excerpt`]/[`EventHub::excerpt_opt`], which
    /// honor `metadata_only`. The name is deliberately specific so a call
    /// site feeding client input through it reads as wrong.
    pub fn cp_diagnostic(&self, text: &str) -> String {
        truncate_with_marker(text, self.max_excerpt_bytes)
    }

    /// Serialize `event` once and offer it to every observer in `namespace`.
    ///
    /// No observers → nothing is serialized and no sequence number is
    /// consumed, which keeps the stream dense from an observer's first frame.
    ///
    /// Ordering: the per-namespace stream lock is held from seq allocation
    /// through the last `try_send`, so frames enter every observer queue in
    /// seq order — concurrent emits in one namespace cannot interleave
    /// (seq=2 enqueued before seq=1 would false-trigger gap detection).
    /// The sends inside the lock are non-blocking `try_send`s to bounded
    /// queues, so the hold time is bounded and the delegation path never
    /// waits on a slow observer. The registry lock is not held here:
    /// `observers()` returns a cloned snapshot.
    pub fn emit(&self, registry: &Registry, namespace: &str, event: CpEvent) {
        let observers = registry.observers(namespace);
        if observers.is_empty() {
            return;
        }
        let stream = {
            let mut g = self.streams.lock();
            Arc::clone(g.entry(namespace.to_string()).or_default())
        };
        let mut seq = stream.lock();
        *seq += 1;
        let params = EventParams {
            seq: *seq,
            ts: chrono::Utc::now(),
            namespace: namespace.to_string(),
            event,
        };
        // Serialized once per emission; registry/hub map locks are not held.
        let text = serde_json::to_string(&JsonRpcNotification::new(
            methods::EVENT,
            serde_json::to_value(&params).expect("serializable"),
        ))
        .expect("serializable");
        for o in observers {
            // Best effort by design: a saturated lobby queue drops the frame.
            if o.tx.try_send(text.clone()).is_err() {
                tracing::debug!(
                    observer = %o.logical_id(),
                    instance = %o.instance_id,
                    seq = params.seq,
                    "observer queue full or closed — event frame dropped"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{AgentType, DeregisterReason};
    use crate::registry::{Instance, OUTBOUND_QUEUE};
    use std::time::Instant;

    fn hub(toml_str: &str) -> EventHub {
        EventHub::new(&toml::from_str::<CpConfig>(toml_str).unwrap())
    }

    fn observer(registry: &Registry, ns: &str, name: &str) -> crate::registry::FrameRx {
        // Generous byte budget: these tests exercise entry-count semantics.
        let (tx, rx) = crate::registry::outbound_channel(64 * 1024 * 1024);
        registry.register(Instance {
            handle: 0,
            namespace: ns.into(),
            name: name.into(),
            agent_type: AgentType::Observer,
            instance_id: format!("i-{name}"),
            labels: Default::default(),
            max_delegated_sessions: 0,
            active_sessions: 0,
            registered_at: Instant::now(),
            last_heartbeat: Instant::now(),
            tx,
        });
        rx
    }

    fn registered(agent: &str) -> CpEvent {
        CpEvent::AgentRegistered {
            agent: agent.into(),
            agent_type: AgentType::Worker,
            instance_id: "i-x".into(),
            labels: Default::default(),
        }
    }

    fn drain(rx: &mut crate::registry::FrameRx) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        while let Ok(text) = rx.try_recv() {
            out.push(serde_json::from_str(&text).unwrap());
        }
        out
    }

    #[test]
    fn seq_is_dense_per_namespace_and_namespaces_are_isolated() {
        let registry = Registry::new();
        let h = hub("");
        let mut prod = observer(&registry, "prod", "lobby-prod");
        let mut dev = observer(&registry, "dev", "lobby-dev");

        // Interleave emissions across the two namespaces.
        h.emit(&registry, "prod", registered("prod/a"));
        h.emit(&registry, "dev", registered("dev/a"));
        h.emit(&registry, "prod", registered("prod/b"));
        h.emit(&registry, "dev", registered("dev/b"));
        h.emit(&registry, "prod", registered("prod/c"));

        let prod_frames = drain(&mut prod);
        let dev_frames = drain(&mut dev);
        assert_eq!(prod_frames.len(), 3);
        assert_eq!(dev_frames.len(), 2);

        // Each observer sees a dense 1..n stream — a global counter would
        // show 1,3,5 here and 2,4 there.
        for (i, f) in prod_frames.iter().enumerate() {
            assert_eq!(f["params"]["seq"], (i + 1) as u64, "prod seq must be dense");
            assert_eq!(f["params"]["namespace"], "prod");
            assert!(f["params"]["agent"].as_str().unwrap().starts_with("prod/"));
        }
        for (i, f) in dev_frames.iter().enumerate() {
            assert_eq!(f["params"]["seq"], (i + 1) as u64, "dev seq must be dense");
            assert_eq!(f["params"]["namespace"], "dev");
            assert!(f["params"]["agent"].as_str().unwrap().starts_with("dev/"));
        }
    }

    #[test]
    fn concurrent_emits_enqueue_in_seq_order() {
        // Regression (review): seq allocation and enqueue must be atomic per
        // namespace. If the stream lock were released between allocating seq
        // and try_send, two concurrent emits could enqueue 2 before 1 and an
        // observer would false-detect a gap. 8 threads × 16 events = 128
        // frames, within OUTBOUND_QUEUE (256) so nothing drops.
        let registry = Registry::new();
        let h = hub("");
        let mut rx = observer(&registry, "prod", "lobby");

        std::thread::scope(|s| {
            for t in 0..8 {
                let h = &h;
                let registry = &registry;
                s.spawn(move || {
                    for i in 0..16 {
                        h.emit(registry, "prod", registered(&format!("prod/t{t}-{i}")));
                    }
                });
            }
        });

        let frames = drain(&mut rx);
        assert_eq!(frames.len(), 128, "no frame may drop below queue capacity");
        for (i, f) in frames.iter().enumerate() {
            assert_eq!(
                f["params"]["seq"],
                (i + 1) as u64,
                "received order must be strictly 1..N — enqueue happened out of seq order"
            );
        }
    }

    #[test]
    fn observer_receives_nothing_from_another_namespace() {
        let registry = Registry::new();
        let h = hub("");
        let mut a = observer(&registry, "ns-a", "lobby-a");
        h.emit(&registry, "ns-b", registered("ns-b/w1"));
        assert!(
            drain(&mut a).is_empty(),
            "cross-namespace leakage into the lobby"
        );
        // ...and ns-b's (absent) observers consumed no ns-a sequence number.
        h.emit(&registry, "ns-a", registered("ns-a/w1"));
        assert_eq!(drain(&mut a)[0]["params"]["seq"], 1);
    }

    #[test]
    fn multiple_observers_in_one_namespace_all_receive_the_same_frame() {
        let registry = Registry::new();
        let h = hub("");
        let mut one = observer(&registry, "prod", "lobby-1");
        let mut two = observer(&registry, "prod", "lobby-2");
        h.emit(&registry, "prod", registered("prod/w1"));
        let a = drain(&mut one);
        let b = drain(&mut two);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0]["params"]["seq"], b[0]["params"]["seq"]);
        assert_eq!(a[0]["params"]["agent"], b[0]["params"]["agent"]);
    }

    #[test]
    fn full_observer_queue_drops_the_frame_without_error() {
        let registry = Registry::new();
        let h = hub("");
        let mut slow = observer(&registry, "prod", "lobby-slow");
        let mut fast = observer(&registry, "prod", "lobby-fast");
        // Saturate the slow observer's bounded queue.
        let slow_inst = registry
            .observers("prod")
            .into_iter()
            .find(|i| i.name == "lobby-slow")
            .unwrap();
        for _ in 0..OUTBOUND_QUEUE {
            slow_inst.tx.try_send("filler".to_string()).unwrap();
        }
        assert!(slow_inst.tx.try_send("overflow".to_string()).is_err());

        // Emission must not panic and must still reach the healthy observer.
        h.emit(&registry, "prod", registered("prod/w1"));
        let fast_frames = drain(&mut fast);
        assert_eq!(fast_frames.len(), 1);
        assert_eq!(fast_frames[0]["params"]["seq"], 1);
        // The slow observer holds only filler; the event frame was dropped.
        let slow_frames: Vec<String> = {
            let mut v = Vec::new();
            while let Ok(t) = slow.try_recv() {
                v.push(t);
            }
            v
        };
        assert_eq!(slow_frames.len(), OUTBOUND_QUEUE);
        assert!(slow_frames.iter().all(|t| t == "filler"));

        // A closed receiver is equally harmless.
        drop(fast);
        h.emit(&registry, "prod", registered("prod/w2"));
    }

    #[test]
    fn excerpt_truncation_is_utf8_boundary_safe_and_within_cap() {
        // 2-byte and 3-byte chars, so many caps land mid-character.
        let body = "héllo 世界 ".repeat(40);
        for cap in 1..=90usize {
            let h = hub(&format!("max_event_excerpt_bytes = {cap}"));
            let out = h.excerpt("prod", &body).unwrap();
            assert!(
                out.len() <= cap,
                "cap {cap} exceeded: {} bytes ({out:?})",
                out.len()
            );
            // Returning a String at all proves no mid-char slice panicked.
            assert!(out.is_char_boundary(out.len()));
        }

        // A representative cap keeps the head and the marker.
        let h = hub("max_event_excerpt_bytes = 96");
        let out = h.excerpt("prod", &body).unwrap();
        assert!(out.contains("truncated by control plane"));
        let head = out.split('\n').next().unwrap();
        assert!(body.starts_with(head), "head must be a prefix of the body");

        // Bodies within the cap are passed through untouched.
        assert_eq!(h.excerpt("prod", "短").unwrap(), "短");
    }

    #[test]
    fn metadata_only_omits_excerpts_but_keeps_metadata() {
        let h = hub(r#"
[namespaces.secret]
metadata_only = true

[namespaces.prod]
max_depth = 2
"#);
        assert!(h.is_metadata_only("secret"));
        assert!(!h.is_metadata_only("prod"));
        assert!(!h.is_metadata_only("unlisted"));

        assert_eq!(h.excerpt("secret", "top secret prompt"), None);
        assert_eq!(h.excerpt_opt("secret", Some("top secret result")), None);
        assert_eq!(
            h.excerpt("prod", "visible prompt"),
            Some("visible prompt".to_string())
        );
        assert_eq!(h.excerpt_opt("prod", None), None);
        // CP-synthesized diagnostics are metadata and survive the knob.
        assert_eq!(h.cp_diagnostic("deadline exceeded"), "deadline exceeded");

        // On the wire, the excerpt key disappears entirely.
        let registry = Registry::new();
        let mut rx = observer(&registry, "secret", "lobby");
        h.emit(
            &registry,
            "secret",
            CpEvent::DelegationRequested {
                delegation_id: "d-1".into(),
                admission: 1,
                from: "secret/koudu".into(),
                to: "secret/worker-1".into(),
                prompt_excerpt: h.excerpt("secret", "top secret prompt"),
                deadline: chrono::Utc::now(),
                chain: vec!["secret/koudu".into()],
            },
        );
        let f = drain(&mut rx);
        assert_eq!(f[0]["params"]["event"], "delegation_requested");
        assert_eq!(f[0]["params"]["from"], "secret/koudu");
        assert!(f[0]["params"].get("prompt_excerpt").is_none());
    }

    #[test]
    fn emitted_frame_is_a_notification_with_expected_envelope() {
        let registry = Registry::new();
        let h = hub("");
        let mut rx = observer(&registry, "prod", "lobby");
        h.emit(
            &registry,
            "prod",
            CpEvent::AgentDeregistered {
                agent: "prod/w1".into(),
                instance_id: "i-1".into(),
                reason: DeregisterReason::LeaseExpired,
            },
        );
        let f = drain(&mut rx);
        assert_eq!(f[0]["jsonrpc"], "2.0");
        assert_eq!(f[0]["method"], "cp/event");
        assert!(f[0].get("id").is_none(), "events are notifications");
        assert_eq!(f[0]["params"]["event"], "agent_deregistered");
        assert_eq!(f[0]["params"]["reason"], "lease_expired");
        assert!(f[0]["params"]["ts"].is_string());
    }
}

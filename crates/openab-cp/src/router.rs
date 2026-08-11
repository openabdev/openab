//! Delegation router: in-flight table, target selection, result routing, and
//! the failure semantics the ADR review required to be explicit:
//!
//! - **Deadline sweep** — an in-flight delegation whose deadline passes is
//!   terminated: the initiator receives a synthesized `timeout` result and
//!   the serving runtime receives a best-effort `cp/cancel` (stop burning
//!   tokens).
//! - **Target disconnect / lease expiry** — in-flight delegations on that
//!   instance fail immediately with `target_disconnected`.
//! - **Initiator disconnect** — its in-flight delegations are cancelled
//!   downstream (best effort); nobody is left to receive the result.
//! - **CP restart** — the table is in-memory; all in-flight delegations
//!   effectively end as initiator-side timeouts. Late `cp/delegate_result`
//!   frames for unknown ids are acknowledged and dropped (logged), so
//!   reconnecting runtimes do not error-loop.
//! - **Saturation** — routing never queues; `SATURATED` is returned
//!   immediately (fast-fail, no hidden buffer).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use tracing::{info, warn};

use crate::config::CpConfig;
use crate::policy::{self, PolicyInput};
use crate::proto::{
    codes, methods, AgentType, CancelParams, DelegateAck, DelegateForward, DelegateParams,
    DelegateResultParams, DelegationStatus, ErrorObject, JsonRpcRequest,
};
use crate::registry::{Instance, Registry, SelectError};

/// One in-flight delegation. Ownership is tracked by CP-generated
/// registration handles, never client-supplied ids (review F1).
#[derive(Clone)]
pub struct InFlight {
    pub delegation_id: String,
    /// Authenticated initiator (`namespace/name`) and its registration handle.
    pub from_logical: String,
    pub from_handle: u64,
    /// Chosen serving instance.
    pub to_logical: String,
    pub to_handle: u64,
    pub deadline: DateTime<Utc>,
    /// CP-constructed chain for THIS delegation (root first, ends with the
    /// initiator). Children extend it.
    pub chain: Vec<String>,
}

pub struct Router {
    inflight: Mutex<BTreeMap<String, InFlight>>,
    /// Serializes the delegate admission sequence (duplicate check → target
    /// selection → capacity reservation → in-flight insert) so concurrent
    /// requests cannot double-admit one id or oversubscribe capacity
    /// (review F2). Delegation rates are LLM-scale; a coarse admission lock
    /// is simple and more than sufficient.
    admission: Mutex<()>,
}

pub enum DelegateOutcome {
    /// Forwarded to the target; ack for the initiator.
    Accepted(DelegateAck),
    /// Rejected; error for the initiator.
    Rejected(ErrorObject),
}

impl Router {
    pub fn new() -> Self {
        Self {
            inflight: Mutex::new(BTreeMap::new()),
            admission: Mutex::new(()),
        }
    }

    /// Handle `cp/delegate` from an authenticated, registered initiator.
    #[allow(clippy::too_many_arguments)]
    pub fn delegate(
        &self,
        cfg: &CpConfig,
        registry: &Registry,
        from_namespace: &str,
        from_name: &str,
        from_type: &AgentType,
        from_handle: u64,
        params: DelegateParams,
        next_rpc_id: u64,
    ) -> DelegateOutcome {
        let now = Utc::now();

        // Admission is one atomic sequence (review F2): duplicate check,
        // parent lookup, target selection, capacity reservation, and
        // in-flight insertion all happen under this guard.
        let _admission = self.admission.lock();

        if self.inflight.lock().contains_key(&params.delegation_id) {
            return DelegateOutcome::Rejected(ErrorObject::new(
                codes::DUPLICATE_DELEGATION,
                format!("delegation {} is already in flight", params.delegation_id),
            ));
        }

        // Selector sanity: exactly one of name/labels.
        let (sel_name, sel_labels) = (params.target.name.as_deref(), params.target.labels.as_ref());
        if sel_name.is_some() == sel_labels.is_some() {
            return DelegateOutcome::Rejected(ErrorObject::new(
                codes::INVALID_PARAMS,
                "target must set exactly one of `name` or `labels`",
            ));
        }

        // Parent linkage: chain and deadline derive from the CP's own table,
        // never from the client. The caller must BE the instance serving the
        // parent delegation — otherwise any runtime knowing a live id could
        // borrow its trusted chain and deadline budget (review F3). Unknown
        // and unauthorized parent ids return the same error (no enumeration).
        let (parent_chain, parent_deadline) = match &params.parent_delegation_id {
            Some(pid) => match self.inflight.lock().get(pid) {
                Some(p) if p.to_handle == from_handle => (p.chain.clone(), Some(p.deadline)),
                _ => {
                    return DelegateOutcome::Rejected(ErrorObject::new(
                        codes::INVALID_PARAMS,
                        format!("parent delegation {pid} is not in flight for this instance"),
                    ))
                }
            },
            None => (Vec::new(), None),
        };

        // Resolve target within the initiator's namespace (v1 boundary).
        let target = match registry.select(from_namespace, sel_name, sel_labels) {
            Ok(i) => i,
            Err(SelectError::NoTarget) => {
                return DelegateOutcome::Rejected(ErrorObject::new(
                    codes::NO_TARGET,
                    "no registered healthy runtime matches the target selector",
                ))
            }
            Err(SelectError::Saturated) => {
                return DelegateOutcome::Rejected(ErrorObject::new(
                    codes::SATURATED,
                    "all matching runtimes are at capacity (CP does not queue; retry later)",
                ))
            }
        };

        // CP-authoritative policy.
        let input = PolicyInput {
            from_namespace,
            from_name,
            from_type,
            target_namespace: &target.namespace,
            target_name: &target.name,
            parent_chain: &parent_chain,
            deadline: params.deadline,
            parent_deadline,
            now,
            max_deadline_secs: cfg.max_deadline_secs,
        };
        if let Err(denial) = policy::check(&input, &cfg.policy_for(from_namespace)) {
            return DelegateOutcome::Rejected(ErrorObject::new(
                codes::POLICY_DENIED,
                denial.to_string(),
            ));
        }

        // Build the forward frame with the CP-stamped chain.
        let from_logical = format!("{from_namespace}/{from_name}");
        let mut chain = parent_chain;
        chain.push(from_logical.clone());
        let forward = DelegateForward {
            delegation_id: params.delegation_id.clone(),
            prompt: params.prompt,
            deadline: params.deadline,
            from: from_logical.clone(),
            chain: chain.clone(),
        };
        let frame = JsonRpcRequest::new(
            next_rpc_id,
            methods::DELEGATE,
            Some(serde_json::to_value(&forward).expect("serializable")),
        );
        let text = serde_json::to_string(&frame).expect("serializable");

        // Reserve capacity and record the in-flight entry BEFORE sending, so
        // an immediately-arriving result finds it (review F2). Roll both
        // back if the send fails.
        registry.adjust_sessions(target.handle, 1);
        let entry = InFlight {
            delegation_id: params.delegation_id.clone(),
            from_logical,
            from_handle,
            to_logical: target.logical_id(),
            to_handle: target.handle,
            deadline: params.deadline,
            chain,
        };
        self.inflight
            .lock()
            .insert(params.delegation_id.clone(), entry.clone());

        if target.tx.try_send(text).is_err() {
            // Disconnected or backpressured beyond its queue: roll back.
            self.inflight.lock().remove(&params.delegation_id);
            registry.adjust_sessions(target.handle, -1);
            return DelegateOutcome::Rejected(ErrorObject::new(
                codes::TARGET_DISCONNECTED,
                "target disconnected or unresponsive during routing",
            ));
        }

        info!(
            delegation = %entry.delegation_id,
            from = %entry.from_logical,
            to = %entry.to_logical,
            chain = ?entry.chain,
            deadline = %entry.deadline,
            "delegation routed"
        );

        DelegateOutcome::Accepted(DelegateAck {
            delegation_id: params.delegation_id,
            assigned_to: target.logical_id(),
        })
    }

    /// Handle `cp/delegate_result` from the serving runtime. Returns the
    /// initiator-bound frame if the delegation is known; unknown ids (e.g.
    /// results arriving after a CP restart) are dropped with a log.
    pub fn complete(
        &self,
        registry: &Registry,
        serving_handle: u64,
        mut params: DelegateResultParams,
        max_result_bytes: usize,
        next_rpc_id: u64,
    ) -> Option<(Instance, String)> {
        let entry = { self.inflight.lock().remove(&params.delegation_id) };
        let entry = match entry {
            Some(e) => e,
            None => {
                warn!(
                    delegation = %params.delegation_id,
                    "result for unknown delegation (late arrival or CP restart) — dropped"
                );
                return None;
            }
        };
        if entry.to_handle != serving_handle {
            // Only the instance the delegation was routed to may complete it.
            warn!(
                delegation = %params.delegation_id,
                expected = entry.to_handle,
                got = serving_handle,
                "result from unexpected instance — dropped, delegation restored"
            );
            self.inflight
                .lock()
                .insert(params.delegation_id.clone(), entry);
            return None;
        }

        registry.adjust_sessions(entry.to_handle, -1);

        // Truncate oversized results (keep the head; delegation already ran).
        if let Some(r) = &params.result {
            if r.len() > max_result_bytes {
                let mut cut = max_result_bytes;
                while !r.is_char_boundary(cut) {
                    cut -= 1;
                }
                params.result = Some(format!(
                    "{}\n…[truncated by control plane: {} of {} bytes]",
                    &r[..cut],
                    cut,
                    r.len()
                ));
            }
        }

        info!(
            delegation = %params.delegation_id,
            status = ?params.status,
            from = %entry.to_logical,
            to = %entry.from_logical,
            "delegation completed"
        );

        let initiator = registry.get(entry.from_handle)?;
        let frame = JsonRpcRequest::new(
            next_rpc_id,
            methods::DELEGATE_RESULT,
            Some(serde_json::to_value(&params).expect("serializable")),
        );
        Some((
            initiator,
            serde_json::to_string(&frame).expect("serializable"),
        ))
    }

    /// Handle `cp/cancel` from the initiator. Returns the frame to forward
    /// to the serving runtime, if the delegation is in flight and owned by
    /// the caller.
    pub fn cancel(
        &self,
        registry: &Registry,
        from_handle: u64,
        params: &CancelParams,
        next_rpc_id: u64,
    ) -> Result<Option<(Instance, String)>, ErrorObject> {
        let entry = { self.inflight.lock().remove(&params.delegation_id) };
        let entry = match entry {
            Some(e) => e,
            None => {
                return Err(ErrorObject::new(
                    codes::INVALID_PARAMS,
                    format!("delegation {} is not in flight", params.delegation_id),
                ))
            }
        };
        if entry.from_handle != from_handle {
            self.inflight
                .lock()
                .insert(params.delegation_id.clone(), entry);
            return Err(ErrorObject::new(
                codes::POLICY_DENIED,
                "only the initiating instance may cancel a delegation",
            ));
        }
        registry.adjust_sessions(entry.to_handle, -1);
        info!(delegation = %params.delegation_id, "delegation cancelled by initiator");
        let target = registry.get(entry.to_handle);
        Ok(target.map(|t| {
            let frame = JsonRpcRequest::new(
                next_rpc_id,
                methods::CANCEL,
                Some(serde_json::to_value(params).expect("serializable")),
            );
            (t, serde_json::to_string(&frame).expect("serializable"))
        }))
    }

    /// Fail every in-flight delegation touching a deregistered instance.
    /// Returns synthesized result/cancel frames to deliver:
    /// - delegations SERVED by the instance → `target_disconnected` result to
    ///   the initiator
    /// - delegations INITIATED by the instance → best-effort `cp/cancel` to
    ///   the serving runtime
    pub fn fail_instance(
        &self,
        registry: &Registry,
        handle: u64,
        rpc_id: &mut impl FnMut() -> u64,
    ) -> Vec<(Instance, String)> {
        let mut affected = Vec::new();
        let entries: Vec<InFlight> = {
            let mut g = self.inflight.lock();
            let ids: Vec<String> = g
                .values()
                .filter(|e| e.to_handle == handle || e.from_handle == handle)
                .map(|e| e.delegation_id.clone())
                .collect();
            ids.iter().filter_map(|id| g.remove(id)).collect()
        };
        for e in entries {
            if e.to_handle == handle {
                // Serving side died → tell the initiator.
                if let Some(init) = registry.get(e.from_handle) {
                    let params = DelegateResultParams {
                        delegation_id: e.delegation_id.clone(),
                        status: DelegationStatus::TargetDisconnected,
                        result: None,
                        error: Some(format!("{} disconnected", e.to_logical)),
                    };
                    let frame = JsonRpcRequest::new(
                        rpc_id(),
                        methods::DELEGATE_RESULT,
                        Some(serde_json::to_value(&params).expect("serializable")),
                    );
                    affected.push((init, serde_json::to_string(&frame).expect("serializable")));
                }
            } else {
                // Initiator died → cancel downstream, free worker capacity.
                registry.adjust_sessions(e.to_handle, -1);
                if let Some(target) = registry.get(e.to_handle) {
                    let params = CancelParams {
                        delegation_id: e.delegation_id.clone(),
                        reason: format!("initiator {} disconnected", e.from_logical),
                    };
                    let frame = JsonRpcRequest::new(
                        rpc_id(),
                        methods::CANCEL,
                        Some(serde_json::to_value(&params).expect("serializable")),
                    );
                    affected.push((target, serde_json::to_string(&frame).expect("serializable")));
                }
            }
            warn!(delegation = %e.delegation_id, handle, "in-flight delegation failed by disconnect");
        }
        affected
    }

    /// Deadline sweep: expire overdue delegations. Returns frames to deliver
    /// (timeout result to the initiator, best-effort cancel to the server).
    pub fn sweep_deadlines(
        &self,
        registry: &Registry,
        now: DateTime<Utc>,
        rpc_id: &mut impl FnMut() -> u64,
    ) -> Vec<(Instance, String)> {
        let overdue: Vec<InFlight> = {
            let mut g = self.inflight.lock();
            let ids: Vec<String> = g
                .values()
                .filter(|e| e.deadline <= now)
                .map(|e| e.delegation_id.clone())
                .collect();
            ids.iter().filter_map(|id| g.remove(id)).collect()
        };
        let mut frames = Vec::new();
        for e in overdue {
            registry.adjust_sessions(e.to_handle, -1);
            warn!(delegation = %e.delegation_id, deadline = %e.deadline, "delegation deadline exceeded");
            if let Some(init) = registry.get(e.from_handle) {
                let params = DelegateResultParams {
                    delegation_id: e.delegation_id.clone(),
                    status: DelegationStatus::Timeout,
                    result: None,
                    error: Some("deadline exceeded".to_string()),
                };
                let frame = JsonRpcRequest::new(
                    rpc_id(),
                    methods::DELEGATE_RESULT,
                    Some(serde_json::to_value(&params).expect("serializable")),
                );
                frames.push((init, serde_json::to_string(&frame).expect("serializable")));
            }
            if let Some(target) = registry.get(e.to_handle) {
                let params = CancelParams {
                    delegation_id: e.delegation_id.clone(),
                    reason: "deadline exceeded".to_string(),
                };
                let frame = JsonRpcRequest::new(
                    rpc_id(),
                    methods::CANCEL,
                    Some(serde_json::to_value(&params).expect("serializable")),
                );
                frames.push((target, serde_json::to_string(&frame).expect("serializable")));
            }
        }
        frames
    }

    /// Chain of an in-flight delegation (for tests/inspection).
    pub fn chain_of(&self, delegation_id: &str) -> Option<Vec<String>> {
        self.inflight
            .lock()
            .get(delegation_id)
            .map(|e| e.chain.clone())
    }

    pub fn inflight_count(&self) -> usize {
        self.inflight.lock().len()
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::TargetSelector;
    use crate::registry::OUTBOUND_QUEUE;
    use chrono::Duration;
    use std::time::Instant;
    use tokio::sync::mpsc;

    fn cfg() -> CpConfig {
        toml::from_str(
            r#"
[[agents]]
key = "kp"
namespace = "prod"
name = "koudu"
type = "primary"

[[agents]]
key = "kw"
namespace = "prod"
name = "worker-1"
type = "worker"
"#,
        )
        .unwrap()
    }

    fn instance(
        ns: &str,
        name: &str,
        ty: AgentType,
        max: u32,
    ) -> (Instance, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel(OUTBOUND_QUEUE);
        (
            Instance {
                handle: 0,
                namespace: ns.into(),
                name: name.into(),
                agent_type: ty,
                instance_id: format!("i-{name}"),
                labels: Default::default(),
                max_delegated_sessions: max,
                active_sessions: 0,
                registered_at: Instant::now(),
                last_heartbeat: Instant::now(),
                tx,
            },
            rx,
        )
    }

    fn delegate_params(id: &str, target: &str, secs: i64) -> DelegateParams {
        DelegateParams {
            delegation_id: id.into(),
            target: TargetSelector {
                name: Some(target.into()),
                labels: None,
            },
            prompt: "do it".into(),
            deadline: Utc::now() + Duration::seconds(secs),
            parent_delegation_id: None,
        }
    }

    struct World {
        cfg: CpConfig,
        registry: Registry,
        router: Router,
        h_primary: u64,
        h_worker: u64,
        worker_rx: mpsc::Receiver<String>,
        primary_rx: mpsc::Receiver<String>,
    }

    fn world() -> World {
        let registry = Registry::new();
        let (p, primary_rx) = instance("prod", "koudu", AgentType::Primary, 4);
        let (w, worker_rx) = instance("prod", "worker-1", AgentType::Worker, 1);
        let h_primary = registry.register(p);
        let h_worker = registry.register(w);
        World {
            cfg: cfg(),
            registry,
            router: Router::new(),
            h_primary,
            h_worker,
            worker_rx,
            primary_rx,
        }
    }

    fn do_delegate(w: &World, params: DelegateParams) -> DelegateOutcome {
        w.router.delegate(
            &w.cfg,
            &w.registry,
            "prod",
            "koudu",
            &AgentType::Primary,
            w.h_primary,
            params,
            1,
        )
    }

    #[test]
    fn happy_path_roundtrip() {
        let mut w = world();
        let out = do_delegate(&w, delegate_params("d-1", "worker-1", 60));
        let ack = match out {
            DelegateOutcome::Accepted(a) => a,
            DelegateOutcome::Rejected(e) => panic!("rejected: {}", e.message),
        };
        assert_eq!(ack.assigned_to, "prod/worker-1");

        let frame = w.worker_rx.try_recv().unwrap();
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(v["method"], "cp/delegate");
        assert_eq!(v["params"]["from"], "prod/koudu");
        assert_eq!(v["params"]["chain"], serde_json::json!(["prod/koudu"]));
        assert_eq!(w.registry.get(w.h_worker).unwrap().active_sessions, 1);

        let result = DelegateResultParams {
            delegation_id: "d-1".into(),
            status: DelegationStatus::Completed,
            result: Some("done".into()),
            error: None,
        };
        let (init, frame) = w
            .router
            .complete(&w.registry, w.h_worker, result, 1024, 2)
            .unwrap();
        assert_eq!(init.handle, w.h_primary);
        assert!(frame.contains("\"completed\""));
        assert_eq!(w.registry.get(w.h_worker).unwrap().active_sessions, 0);
        assert_eq!(w.router.inflight_count(), 0);
    }

    #[test]
    fn inflight_exists_before_target_receives_frame() {
        // Review F2: an immediately-arriving result must find the entry.
        let mut w = world();
        assert!(matches!(
            do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        // Complete BEFORE draining the worker's queue — entry must exist.
        let result = DelegateResultParams {
            delegation_id: "d-1".into(),
            status: DelegationStatus::Completed,
            result: Some("instant".into()),
            error: None,
        };
        assert!(w
            .router
            .complete(&w.registry, w.h_worker, result, 1024, 2)
            .is_some());
        w.worker_rx.try_recv().unwrap();
    }

    #[test]
    fn send_failure_rolls_back_reservation() {
        // Close the worker's rx so try_send fails, then verify rollback.
        let mut w = world();
        w.worker_rx.close();
        match do_delegate(&w, delegate_params("d-1", "worker-1", 60)) {
            DelegateOutcome::Rejected(e) => assert_eq!(e.code, codes::TARGET_DISCONNECTED),
            _ => panic!("expected TARGET_DISCONNECTED"),
        }
        assert_eq!(w.router.inflight_count(), 0);
        assert_eq!(
            w.registry.get(w.h_worker).unwrap().active_sessions,
            0,
            "capacity reservation must be rolled back"
        );
    }

    #[test]
    fn duplicate_delegation_id_rejected() {
        let w = world();
        assert!(matches!(
            do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        match do_delegate(&w, delegate_params("d-1", "worker-1", 60)) {
            DelegateOutcome::Rejected(e) => assert_eq!(e.code, codes::DUPLICATE_DELEGATION),
            _ => panic!("expected rejection"),
        }
    }

    #[test]
    fn saturation_fast_fails() {
        let w = world(); // worker max = 1
        assert!(matches!(
            do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        match do_delegate(&w, delegate_params("d-2", "worker-1", 60)) {
            DelegateOutcome::Rejected(e) => assert_eq!(e.code, codes::SATURATED),
            _ => panic!("expected SATURATED"),
        }
    }

    #[test]
    fn selector_must_be_exactly_one() {
        let w = world();
        let mut p = delegate_params("d-1", "worker-1", 60);
        p.target.labels = Some(Default::default());
        match do_delegate(&w, p) {
            DelegateOutcome::Rejected(e) => assert_eq!(e.code, codes::INVALID_PARAMS),
            _ => panic!(),
        }
        let mut p2 = delegate_params("d-2", "worker-1", 60);
        p2.target.name = None;
        match do_delegate(&w, p2) {
            DelegateOutcome::Rejected(e) => assert_eq!(e.code, codes::INVALID_PARAMS),
            _ => panic!(),
        }
    }

    #[test]
    fn policy_denial_maps_to_error_code() {
        let w = world();
        let out = w.router.delegate(
            &w.cfg,
            &w.registry,
            "prod",
            "worker-1",
            &AgentType::Worker,
            w.h_worker,
            delegate_params("d-1", "koudu", 60),
            1,
        );
        match out {
            DelegateOutcome::Rejected(e) => assert_eq!(e.code, codes::POLICY_DENIED),
            _ => panic!("expected POLICY_DENIED"),
        }
    }

    #[test]
    fn unknown_target_is_no_target() {
        let w = world();
        match do_delegate(&w, delegate_params("d-1", "ghost", 60)) {
            DelegateOutcome::Rejected(e) => assert_eq!(e.code, codes::NO_TARGET),
            _ => panic!(),
        }
    }

    #[test]
    fn result_from_wrong_handle_dropped_and_restored() {
        let w = world();
        assert!(matches!(
            do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        let result = DelegateResultParams {
            delegation_id: "d-1".into(),
            status: DelegationStatus::Completed,
            result: Some("spoofed".into()),
            error: None,
        };
        // h_primary is a valid handle but NOT the serving instance.
        assert!(w
            .router
            .complete(&w.registry, w.h_primary, result, 1024, 2)
            .is_none());
        assert_eq!(w.router.inflight_count(), 1);
    }

    #[test]
    fn late_result_after_restart_dropped() {
        let w = world();
        let result = DelegateResultParams {
            delegation_id: "d-unknown".into(),
            status: DelegationStatus::Completed,
            result: None,
            error: None,
        };
        assert!(w
            .router
            .complete(&w.registry, w.h_worker, result, 1024, 2)
            .is_none());
    }

    #[test]
    fn oversized_result_truncated() {
        let mut w = world();
        assert!(matches!(
            do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        w.worker_rx.try_recv().unwrap();
        let result = DelegateResultParams {
            delegation_id: "d-1".into(),
            status: DelegationStatus::Completed,
            result: Some("x".repeat(100)),
            error: None,
        };
        let (_, frame) = w
            .router
            .complete(&w.registry, w.h_worker, result, 10, 2)
            .unwrap();
        assert!(frame.contains("truncated by control plane"));
    }

    #[test]
    fn deadline_sweep_times_out_and_cancels() {
        let mut w = world();
        assert!(matches!(
            do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        w.worker_rx.try_recv().unwrap();

        let mut id = 100u64;
        let mut next = || {
            id += 1;
            id
        };
        assert!(w
            .router
            .sweep_deadlines(&w.registry, Utc::now(), &mut next)
            .is_empty());
        let frames =
            w.router
                .sweep_deadlines(&w.registry, Utc::now() + Duration::seconds(120), &mut next);
        assert_eq!(frames.len(), 2);
        assert_eq!(w.router.inflight_count(), 0);
        assert_eq!(w.registry.get(w.h_worker).unwrap().active_sessions, 0);

        for (inst, frame) in frames {
            let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
            match v["method"].as_str().unwrap() {
                "cp/delegate_result" => {
                    assert_eq!(inst.handle, w.h_primary);
                    assert_eq!(v["params"]["status"], "timeout");
                }
                "cp/cancel" => assert_eq!(inst.handle, w.h_worker),
                m => panic!("unexpected method {m}"),
            }
        }
    }

    #[test]
    fn worker_disconnect_fails_delegation_to_initiator() {
        let mut w = world();
        assert!(matches!(
            do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        w.worker_rx.try_recv().unwrap();
        w.registry.deregister(w.h_worker);

        let mut id = 0u64;
        let mut next = || {
            id += 1;
            id
        };
        let frames = w.router.fail_instance(&w.registry, w.h_worker, &mut next);
        assert_eq!(frames.len(), 1);
        let (inst, frame) = &frames[0];
        assert_eq!(inst.handle, w.h_primary);
        assert!(frame.contains("target_disconnected"));
        assert_eq!(w.router.inflight_count(), 0);
        inst.tx.try_send(frame.clone()).unwrap();
        assert!(w.primary_rx.try_recv().unwrap().contains("d-1"));
    }

    #[test]
    fn initiator_disconnect_cancels_downstream() {
        let mut w = world();
        assert!(matches!(
            do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        w.worker_rx.try_recv().unwrap();
        w.registry.deregister(w.h_primary);

        let mut id = 0u64;
        let mut next = || {
            id += 1;
            id
        };
        let frames = w.router.fail_instance(&w.registry, w.h_primary, &mut next);
        assert_eq!(frames.len(), 1);
        let (inst, frame) = &frames[0];
        assert_eq!(inst.handle, w.h_worker);
        assert!(frame.contains("cp/cancel"));
        assert_eq!(w.registry.get(w.h_worker).unwrap().active_sessions, 0);
    }

    #[test]
    fn cancel_only_by_initiator() {
        let w = world();
        assert!(matches!(
            do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        let params = CancelParams {
            delegation_id: "d-1".into(),
            reason: "changed my mind".into(),
        };
        let err = w
            .router
            .cancel(&w.registry, w.h_worker, &params, 5)
            .unwrap_err();
        assert_eq!(err.code, codes::POLICY_DENIED);
        assert_eq!(w.router.inflight_count(), 1);
        let fwd = w
            .router
            .cancel(&w.registry, w.h_primary, &params, 6)
            .unwrap();
        let (inst, frame) = fwd.unwrap();
        assert_eq!(inst.handle, w.h_worker);
        assert!(frame.contains("cp/cancel"));
        assert_eq!(w.router.inflight_count(), 0);
        assert_eq!(w.registry.get(w.h_worker).unwrap().active_sessions, 0);
    }

    #[test]
    fn chain_extends_through_parent_and_foreign_parent_rejected() {
        let w = world();
        let cfg: CpConfig = toml::from_str(
            r#"
[[agents]]
key = "kp"
namespace = "prod"
name = "koudu"
type = "primary"

[namespaces.prod]
max_depth = 5
allow_worker_initiation = true
"#,
        )
        .unwrap();
        let (w2, _rx2) = instance("prod", "worker-2", AgentType::Worker, 1);
        let h_w2 = w.registry.register(w2);

        assert!(matches!(
            w.router.delegate(
                &cfg,
                &w.registry,
                "prod",
                "koudu",
                &AgentType::Primary,
                w.h_primary,
                delegate_params("d-root", "worker-1", 120),
                1,
            ),
            DelegateOutcome::Accepted(_)
        ));
        assert_eq!(
            w.router.chain_of("d-root").unwrap(),
            vec!["prod/koudu".to_string()]
        );

        // Review F3: worker-2 (NOT serving d-root) tries to borrow d-root
        // as parent — rejected.
        let mut foreign = delegate_params("d-foreign", "worker-2", 60);
        foreign.parent_delegation_id = Some("d-root".into());
        match w.router.delegate(
            &cfg,
            &w.registry,
            "prod",
            "worker-2",
            &AgentType::Worker,
            h_w2,
            foreign,
            2,
        ) {
            DelegateOutcome::Rejected(e) => {
                assert_eq!(e.code, codes::INVALID_PARAMS);
                assert!(e.message.contains("not in flight for this instance"));
            }
            _ => panic!("foreign parent must be rejected"),
        }

        // worker-1 (serving d-root) delegates a legitimate child to worker-2.
        let mut child = delegate_params("d-child", "worker-2", 60);
        child.parent_delegation_id = Some("d-root".into());
        assert!(matches!(
            w.router.delegate(
                &cfg,
                &w.registry,
                "prod",
                "worker-1",
                &AgentType::Worker,
                w.h_worker,
                child,
                3,
            ),
            DelegateOutcome::Accepted(_)
        ));
        assert_eq!(
            w.router.chain_of("d-child").unwrap(),
            vec!["prod/koudu".to_string(), "prod/worker-1".to_string()]
        );

        // Cycle: worker-2 delegating back to koudu is rejected.
        let mut cyc = delegate_params("d-cyc", "koudu", 30);
        cyc.parent_delegation_id = Some("d-child".into());
        match w.router.delegate(
            &cfg,
            &w.registry,
            "prod",
            "worker-2",
            &AgentType::Worker,
            h_w2,
            cyc,
            4,
        ) {
            DelegateOutcome::Rejected(e) => {
                assert_eq!(e.code, codes::POLICY_DENIED);
                assert!(e.message.contains("cycle"), "{}", e.message);
            }
            _ => panic!("expected cycle rejection"),
        }
    }
}

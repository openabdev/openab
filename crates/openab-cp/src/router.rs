//! Delegation router: in-flight table, target selection, result routing, and
//! the failure semantics the ADR requires to be explicit rather than implied:
//!
//! - **Deadline sweep** — an in-flight delegation whose deadline passes is
//!   terminated: the initiator receives a synthesized `timeout` result and
//!   the serving runtime receives a best-effort `cp/cancel` (stop burning
//!   tokens).
//! - **Target disconnect / lease expiry** — in-flight delegations on that
//!   instance fail immediately with `target_disconnected`.
//! - **Initiator disconnect** — its in-flight delegations are cancelled
//!   downstream (best effort); nobody is left to receive the result.
//! - **CP restart** — the table dies with the process, and the connections
//!   die with it, so the CP cannot synthesize anything: in-flight delegations
//!   end as initiator-side timeouts against the already-propagated deadline.
//!   Late `cp/delegate_result` frames for unknown ids are acknowledged and
//!   dropped (logged), so reconnecting runtimes do not error-loop.
//! - **Saturation** — routing never queues; `SATURATED` is returned
//!   immediately (fast-fail, no hidden buffer).
//!
//! # Terminal frames
//!
//! Ending a delegation is a two-sided event: a frame goes out on the wire and
//! CP state is committed. The commit is exact — it claims the one admission it
//! delivered a result for (key + serving handle + [`InFlight::generation`]) or
//! nothing at all — so CP state stays consistent under any interleaving.
//!
//! The wire is a different matter: a `completed` result racing the deadline
//! sweep's synthesized `timeout` can put TWO terminal frames on the wire for
//! one `delegation_id`. v1 resolves that by contract instead of CP-side
//! suppression (which would need per-id terminal state the CP deliberately
//! does not keep): **the first terminal frame for a `delegation_id` wins**, and
//! initiators MUST ignore later ones. See the v1 contract amendments in
//! `docs/adr/agent-control-plane.md`.
//!
//! # Lock hierarchy
//!
//! The router holds two locks and acquires them in ONE order only:
//!
//! ```text
//! admission  →  inflight        (never the reverse)
//! ```
//!
//! `admission` serializes the whole delegate admission sequence; `inflight`
//! guards the in-flight table itself and is taken for short, self-contained
//! critical sections. Every path that needs both — only `delegate` does —
//! takes `admission` first and then `inflight`, possibly several times.
//! No path may take `inflight` and then reach for `admission`: because
//! `delegate` holds `admission` across `inflight` acquisitions, doing so
//! would close a deadlock cycle. Paths that need only the table
//! (`complete`, `cancel`, `fail_instance`, `sweep_deadlines`, `chain_of`)
//! take `inflight` alone and never touch `admission`.
//!
//! Registry access is a third, independent lock owned by [`Registry`]. It is
//! always acquired and released *outside* an `inflight` critical section
//! (e.g. `registry.get(...)` completes before the table is locked), so it
//! does not participate in this hierarchy.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

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
/// registration handles, never client-supplied ids: a colliding
/// `instance_id` on another connection can neither complete nor cancel this
/// delegation.
#[derive(Clone)]
pub struct InFlight {
    /// Namespace that owns this delegation — part of its identity, not just a
    /// lookup key: `delegation_id` is client-supplied and only unique within
    /// the namespace that produced it.
    ///
    /// Stored on the entry because the delegation outlives the request that
    /// created it, and the paths that end it without a client request —
    /// `fail_instance` and `sweep_deadlines` — have no namespace of their own
    /// to work from. Its consumer is the observer event layer added by the
    /// next PR in this stack (per-namespace `cp/event` fan-out reads
    /// `e.namespace` in exactly those two paths), so the field is part of the
    /// entry's contract rather than an unused remnant.
    pub namespace: String,
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
    /// CP-generated, never-reused admission stamp. `(namespace,
    /// delegation_id)` is NOT a stable identity over time: the id is
    /// client-supplied, and cancel-then-retry — a natural client pattern —
    /// legitimately re-admits the same id, which with a single replica routes
    /// to the same serving instance again. Key plus serving handle therefore
    /// cannot distinguish the entry a two-phase completion peeked from a
    /// later, unrelated admission wearing the same clothes (an ABA race).
    ///
    /// The generation makes that distinction total: it is minted once per
    /// admission from a monotonic counter and never reused, so the commit
    /// step claims the entry it actually delivered a result for, or nothing.
    pub generation: u64,
}

/// In-flight table key: `(namespace, delegation_id)`.
///
/// Keying on the client-supplied `delegation_id` alone made one namespace's
/// ids observable from another: a colliding id was denied with
/// `DUPLICATE_DELEGATION`, and `cp/cancel` distinguished "no such id" from
/// "someone else's live id" — a cross-tenant existence oracle. The composite
/// key confines both to the namespace that owns the id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct DelegationKey {
    namespace: String,
    delegation_id: String,
}

impl DelegationKey {
    fn new(namespace: &str, delegation_id: &str) -> Self {
        Self {
            namespace: namespace.to_string(),
            delegation_id: delegation_id.to_string(),
        }
    }
}

pub struct Router {
    inflight: Mutex<BTreeMap<DelegationKey, InFlight>>,
    /// Serializes the delegate admission sequence (duplicate check → target
    /// selection → capacity reservation → in-flight insert) so concurrent
    /// requests cannot double-admit one id or oversubscribe capacity: without
    /// it, two racing delegates both see a free slot and both reserve it.
    /// Delegation rates are LLM-scale; a coarse admission lock is simple and
    /// more than sufficient.
    admission: Mutex<()>,
    /// Source of [`InFlight::generation`] stamps. Monotonic and never reset
    /// (the table dies with the process, so a restart cannot collide with
    /// anything still in flight): every admission gets a value no earlier or
    /// later admission has worn.
    next_generation: AtomicU64,
}

pub enum DelegateOutcome {
    /// Forwarded to the target; ack for the initiator.
    Accepted(DelegateAck),
    /// Rejected; error for the initiator.
    Rejected(ErrorObject),
}

/// Result of looking up an in-flight delegation on behalf of its claimed
/// initiator and removing it if the claim holds.
///
/// The whole check happens under ONE acquisition of the in-flight lock, which
/// is the property that matters: an earlier version removed the entry,
/// validated ownership, then reinserted it on refusal, and a genuine frame
/// landing in that window saw an empty table and was dropped as "unknown id",
/// leaving the delegation to stall until its deadline. Here the entry is
/// either removed because the caller owns it, or never touched at all.
///
/// Single-phase by construction — the caller acts on the returned entry
/// without going back to the table — so no window exists in which the id
/// could be re-admitted under the caller's feet. Contrast the two-phase
/// completion path, which needs [`InFlight::generation`] for exactly that
/// reason. `cp/cancel` is this helper's only caller.
enum Claim {
    /// The caller initiated it; the entry has already been removed.
    Owned(InFlight),
    /// The entry exists but was initiated by another instance. Left in place.
    WrongOwner {
        namespace: String,
        /// Handle of the instance that does own it (CP-side logs only — it is
        /// never disclosed to the caller).
        owner_handle: u64,
    },
    /// No entry for `(namespace, delegation_id)`.
    NotFound { namespace: String },
    /// The calling connection has no registration: it was swept (lease
    /// expiry) or never registered, so it has no namespace to look in.
    Unregistered,
}

/// Phase-1 snapshot of a completion: the entry as it existed at peek time,
/// including its [`InFlight::generation`] stamp, or why no result can be
/// delivered for it. A peek never removes anything.
enum Peek {
    /// The caller is the instance the delegation was routed to.
    Serving(InFlight),
    /// The entry exists but another instance serves it. Left in place — a
    /// non-owner frame must never make the delegation momentarily invisible
    /// to a genuine result or to the deadline sweep.
    Foreign {
        /// CP-side logs only; never disclosed to the caller.
        owner_handle: u64,
    },
    /// No entry for `(namespace, delegation_id)`.
    Unknown,
}

/// Phase-2 result of committing a delivered completion (see
/// [`Router::commit_completion`]).
#[derive(Debug, PartialEq, Eq)]
enum Commit {
    /// The peeked admission was still the live one: entry removed and the
    /// serving instance's capacity released, exactly once.
    Claimed,
    /// The entry is gone — a concurrent cancel, sweep, disconnect, or a
    /// duplicate result's commit removed it. Whichever path removed it
    /// released the capacity; this one must not decrement again.
    Vanished,
    /// A DIFFERENT admission holds `(namespace, delegation_id)` now: the id
    /// was removed and re-admitted between peek and commit (cancel-then-retry
    /// routed back to the same worker is the ordinary way this happens, and
    /// with a single replica it is the *only* way it happens). The live entry
    /// and its capacity are left untouched: claiming it would erase a
    /// delegation that is genuinely running and silently drop its real result
    /// later.
    Superseded {
        /// Generation now holding the id (CP-side logs only).
        generation: u64,
    },
}

/// Outcome of a `cp/delegate_result` frame (see [`Router::complete`]).
///
/// Wire delivery and state commit are distinct events: the initiator can have
/// received the result while the CP's own bookkeeping was concluded by
/// somebody else (a concurrent cancel, sweep, or disconnect). Collapsing the
/// two hid whether this frame is the one that ended the delegation.
#[derive(Debug, PartialEq, Eq)]
pub enum CompleteOutcome {
    /// The result reached the initiator's queue.
    Delivered {
        /// Whether THIS frame also committed the state transition — removed
        /// the in-flight entry it peeked and released the serving instance's
        /// capacity. `false` means a concurrent path had already ended the
        /// delegation (or its id was re-admitted), so nothing was changed
        /// here; the frame was still delivered.
        committed: bool,
    },
    /// The frame was refused or the delegation is unknown (wrong owner,
    /// unknown id, unregistered caller, or the initiator is gone). Nothing
    /// changed; each case is logged.
    Dropped,
    /// The initiator's bounded outbound queue refused the terminal result.
    /// The entry is still in flight: the caller must treat the initiator as
    /// disconnected (close its connection), whose teardown then fails the
    /// delegation through `fail_instance` — capacity is released exactly
    /// once and the serving runtime receives `cp/cancel`.
    InitiatorStalled {
        /// Registration handle of the stalled initiator.
        initiator_handle: u64,
    },
}

impl Router {
    pub fn new() -> Self {
        Self {
            inflight: Mutex::new(BTreeMap::new()),
            admission: Mutex::new(()),
            next_generation: AtomicU64::new(0),
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

        // Admission is one atomic sequence: duplicate check, parent lookup,
        // target selection, capacity reservation, and in-flight insertion all
        // happen under this guard, so two racing delegates can neither
        // double-admit an id nor both claim the last free slot.
        let _admission = self.admission.lock();

        // Delegation identity is namespace-scoped: the same id in another
        // namespace is a different delegation, so it neither collides here
        // nor leaks its existence.
        let key = DelegationKey::new(from_namespace, &params.delegation_id);
        if self.inflight.lock().contains_key(&key) {
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
        // borrow its trusted chain and deadline budget. The lookup is
        // namespace-scoped. Unknown and unauthorized parent ids return the
        // same error (no enumeration).
        let (parent_chain, parent_deadline) = match &params.parent_delegation_id {
            Some(pid) => {
                let parent_key = DelegationKey::new(from_namespace, pid);
                match self.inflight.lock().get(&parent_key) {
                    Some(p) if p.to_handle == from_handle => (p.chain.clone(), Some(p.deadline)),
                    _ => {
                        return DelegateOutcome::Rejected(ErrorObject::new(
                            codes::INVALID_PARAMS,
                            format!("parent delegation {pid} is not in flight for this instance"),
                        ))
                    }
                }
            }
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
        // an immediately-arriving result finds it. Roll both back if the send
        // fails.
        //
        // Minted BEFORE the capacity reservation so exhaustion cannot leave a
        // reserved slot behind. `fetch_add` on an exhausted counter would wrap
        // and re-issue generation values, silently recreating the ABA this
        // stamp exists to prevent — so exhaustion fails the admission instead.
        // (Unreachable in practice: one admission per nanosecond exhausts a
        // u64 after ~584 years; this is fail-closed insurance, not a path.)
        // `fetch_update` rather than `fetch_add`: an exhausted counter must
        // NOT be written (fetch_add would wrap the atomic itself and re-issue
        // generations from 0). Refusal leaves the counter parked at the
        // ceiling, so every later admission is refused too — fail closed,
        // permanently, with no wrapping path.
        let generation =
            match self
                .next_generation
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |g| {
                    if g >= u64::MAX - 1 {
                        None
                    } else {
                        Some(g + 1)
                    }
                }) {
                Ok(prev) => prev + 1,
                Err(_) => {
                    tracing::error!("delegation generation space exhausted — refusing admission");
                    // -32603 = JSON-RPC internal error; no protocol-specific code
                    // is warranted for a condition that cannot occur in a
                    // process's realistic lifetime.
                    return DelegateOutcome::Rejected(ErrorObject::new(
                        -32603,
                        "control plane generation space exhausted; restart the CP",
                    ));
                }
            };
        registry.adjust_sessions(target.handle, 1);
        let entry = InFlight {
            namespace: from_namespace.to_string(),
            delegation_id: params.delegation_id.clone(),
            from_logical,
            from_handle,
            to_logical: target.logical_id(),
            to_handle: target.handle,
            deadline: params.deadline,
            chain,
            // Stamped under the admission lock, so every admission — including
            // a re-admission of an id that was just cancelled — is
            // distinguishable from every other for the life of the process.
            generation,
        };
        self.inflight.lock().insert(key.clone(), entry.clone());

        if target.tx.try_send(text).is_err() {
            // Disconnected or backpressured beyond its queue: roll back.
            //
            // Roll back only what this call still owns. `fail_instance` and
            // `sweep_deadlines` take the in-flight lock without the admission
            // lock, so they can remove this very entry between the insert
            // above and this branch — and whoever removes an entry also
            // releases its capacity reservation. Decrementing here after a
            // concurrent removal would double-release: the saturating math
            // hides the underflow and `saturated()` then admits new work to
            // an instance that is actually full.
            //
            // Matched on the generation, not just the key: the admission lock
            // happens to rule out a re-admission of this id while we are
            // here, but the rollback does not need that argument to be
            // correct — it removes the exact entry it inserted or nothing.
            if self.remove_generation(&key, entry.generation).is_some() {
                registry.adjust_sessions(target.handle, -1);
            }
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

    /// Remove `key` only if it still holds `generation` — the exact admission
    /// the caller is acting for — under one lock acquisition. Any other entry
    /// (or none) is left untouched.
    fn remove_generation(&self, key: &DelegationKey, generation: u64) -> Option<InFlight> {
        let mut g = self.inflight.lock();
        match g.get(key) {
            Some(e) if e.generation == generation => g.remove(key),
            _ => None,
        }
    }

    /// Look up `delegation_id` in the caller's namespace, assert the caller
    /// initiated it, and remove the entry if so — all under one acquisition of
    /// the in-flight lock (see [`Claim`]).
    ///
    /// The namespace is taken from the caller's authenticated registration,
    /// never from the frame, so a delegation id can only ever be resolved
    /// inside the namespace of the connection that named it.
    fn claim(&self, registry: &Registry, handle: u64, delegation_id: &str) -> Claim {
        // Registry lookup completes before the in-flight lock is taken; the
        // two locks are never held together (see the lock hierarchy above).
        let namespace = match registry.get(handle) {
            Some(i) => i.namespace,
            None => return Claim::Unregistered,
        };
        let key = DelegationKey::new(&namespace, delegation_id);
        let mut g = self.inflight.lock();
        let owner_handle = match g.get(&key) {
            Some(e) => e.from_handle,
            None => return Claim::NotFound { namespace },
        };
        if owner_handle != handle {
            return Claim::WrongOwner {
                namespace,
                owner_handle,
            };
        }
        let entry = g.remove(&key).expect("present under the same lock");
        Claim::Owned(entry)
    }

    /// Phase 1 of a completion: snapshot the entry for `(namespace,
    /// delegation_id)` and assert `serving_handle` is the instance it was
    /// routed to — under one in-flight lock acquisition, removing nothing.
    ///
    /// The returned [`InFlight`] carries the [`InFlight::generation`] the
    /// commit step must match, so delivery can happen outside the lock without
    /// the commit ever being able to claim a different admission.
    fn peek_for_completion(
        &self,
        namespace: &str,
        delegation_id: &str,
        serving_handle: u64,
    ) -> Peek {
        let key = DelegationKey::new(namespace, delegation_id);
        let g = self.inflight.lock();
        match g.get(&key) {
            Some(e) if e.to_handle == serving_handle => Peek::Serving(e.clone()),
            Some(e) => Peek::Foreign {
                owner_handle: e.to_handle,
            },
            None => Peek::Unknown,
        }
    }

    /// Phase 2 of a completion: end the delegation `peeked` describes.
    ///
    /// Under ONE in-flight lock acquisition, the entry is removed and the
    /// serving instance's capacity released only if the live entry is still
    /// the same admission — key, serving handle, AND generation all match.
    /// Anything else is left strictly untouched, including its capacity:
    ///
    /// - a concurrent cancel/sweep/disconnect already ended it → [`Commit::Vanished`],
    ///   and that path already released the capacity (releasing it here too
    ///   would let `saturated()` admit work to a full instance);
    /// - the id was re-admitted in the meantime → [`Commit::Superseded`]. This
    ///   is the ABA case that made a stale commit destructive: matching on
    ///   key + serving handle alone, a commit for delegation *n* would remove
    ///   the live entry of delegation *n+1* (cancel-then-retry re-admits the
    ///   same id, and with one replica it routes to the same worker), making a
    ///   running delegation invisible to its own genuine result and to the
    ///   sweep, and wrongly freeing its slot.
    fn commit_completion(&self, registry: &Registry, peeked: &InFlight) -> Commit {
        let key = DelegationKey::new(&peeked.namespace, &peeked.delegation_id);
        let removed = {
            let mut g = self.inflight.lock();
            match g.get(&key) {
                Some(e) if e.to_handle == peeked.to_handle && e.generation == peeked.generation => {
                    g.remove(&key).expect("present under the same lock")
                }
                Some(e) => {
                    return Commit::Superseded {
                        generation: e.generation,
                    }
                }
                None => return Commit::Vanished,
            }
        };
        registry.adjust_sessions(removed.to_handle, -1);
        Commit::Claimed
    }

    /// Handle `cp/delegate_result` from the serving runtime.
    ///
    /// The terminal result is the one frame that must never be silently
    /// dropped, so delivery happens in two phases:
    ///
    /// 1. **Peek** — validate ownership under one in-flight lock acquisition
    ///    without removing the entry ([`Router::peek_for_completion`]), then
    ///    build and `try_send` the initiator-bound frame.
    /// 2. **Commit** — only after the initiator's queue accepted the frame,
    ///    end the delegation ([`Router::commit_completion`]): remove the
    ///    entry and release the serving instance's capacity, but only if the
    ///    live entry is still the very admission that was peeked (key +
    ///    serving handle + [`InFlight::generation`]).
    ///
    /// If the initiator's bounded queue refuses the frame, the entry stays
    /// in flight and [`CompleteOutcome::InitiatorStalled`] tells the caller
    /// to treat the initiator as disconnected (per the bounded-queue
    /// contract): its teardown runs `fail_instance`, which releases capacity
    /// exactly once and sends `cp/cancel` to the serving runtime.
    ///
    /// Nothing outside the commit's exact-match window is touched, so the
    /// peek-send window cannot corrupt CP state: a concurrent cancel, sweep,
    /// or disconnect that already ended the delegation leaves this frame with
    /// `Delivered { committed: false }`, and an id re-admitted in the window
    /// keeps its own live entry and capacity.
    ///
    /// What the window *can* still produce is more than one terminal frame on
    /// the wire for one `delegation_id` — a `completed` result racing the
    /// sweep's synthesized `timeout`, or two duplicate results both passing
    /// the peek. That is resolved by contract, not by CP-side suppression:
    /// initiators MUST treat the FIRST terminal frame for a `delegation_id` as
    /// authoritative and ignore later ones (see "first terminal frame wins"
    /// in the ADR's v1 contract amendments).
    ///
    /// Only the instance the delegation was routed to may complete it; a
    /// non-owner frame can never make the delegation momentarily invisible
    /// to a genuine result or to the deadline sweep.
    pub fn complete(
        &self,
        registry: &Registry,
        serving_handle: u64,
        mut params: DelegateResultParams,
        max_result_bytes: usize,
        next_rpc_id: u64,
    ) -> CompleteOutcome {
        // Phase 1 — peek: validate without removing. Removing before the
        // send would make a refused send unrecoverable (silent loss of a
        // computed result while the serving side is acked as delivered).
        let namespace = match registry.get(serving_handle) {
            Some(i) => i.namespace,
            None => {
                warn!(
                    handle = serving_handle,
                    delegation = %params.delegation_id,
                    "result from an unregistered connection — dropped"
                );
                return CompleteOutcome::Dropped;
            }
        };
        let entry =
            match self.peek_for_completion(&namespace, &params.delegation_id, serving_handle) {
                Peek::Serving(e) => e,
                Peek::Foreign { owner_handle } => {
                    // Only the instance the delegation was routed to may
                    // complete it. The entry stays exactly where it is.
                    warn!(
                        delegation = %params.delegation_id,
                        namespace = %namespace,
                        expected = owner_handle,
                        got = serving_handle,
                        "result from unexpected instance — dropped, delegation untouched"
                    );
                    return CompleteOutcome::Dropped;
                }
                Peek::Unknown => {
                    warn!(
                        delegation = %params.delegation_id,
                        namespace = %namespace,
                        "result for unknown delegation (late arrival or CP restart) — dropped"
                    );
                    return CompleteOutcome::Dropped;
                }
            };

        // Truncate oversized results (keep the head; delegation already
        // ran). The marker counts against the cap: the final value never
        // exceeds max_result_bytes.
        if let Some(r) = &params.result {
            if r.len() > max_result_bytes {
                let marker = format!("\n…[truncated by control plane: {} bytes total]", r.len());
                let budget = max_result_bytes.saturating_sub(marker.len());
                let cut = floor_char_boundary(r, budget);
                let mut out = format!("{}{}", &r[..cut], marker);
                if out.len() > max_result_bytes {
                    // Degenerate tiny cap: keep whatever fits.
                    out.truncate(floor_char_boundary(&out, max_result_bytes));
                }
                params.result = Some(out);
            }
        }

        let Some(initiator) = registry.get(entry.from_handle) else {
            // The initiator deregistered concurrently: its `fail_instance`
            // pass removes this entry, releases capacity, and cancels the
            // serving side — nothing to do here.
            warn!(
                delegation = %params.delegation_id,
                "result for a delegation whose initiator is gone — dropped"
            );
            return CompleteOutcome::Dropped;
        };
        let frame = JsonRpcRequest::new(
            next_rpc_id,
            methods::DELEGATE_RESULT,
            Some(serde_json::to_value(&params).expect("serializable")),
        );
        let text = serde_json::to_string(&frame).expect("serializable");

        if initiator.tx.try_send(text).is_err() {
            // Bounded-queue contract: a peer that cannot drain its queue is
            // treated as disconnected, never silently skipped. The entry
            // stays in flight; the caller closes the initiator, whose
            // teardown fails the delegation over the `fail_instance` path.
            warn!(
                delegation = %params.delegation_id,
                initiator = %entry.from_logical,
                "initiator queue full — terminal result refused, treating initiator as disconnected"
            );
            return CompleteOutcome::InitiatorStalled {
                initiator_handle: entry.from_handle,
            };
        }

        // Phase 2 — commit. Claims ONLY the admission that was peeked; see
        // `commit_completion` for why key + serving handle is not enough.
        let committed = match self.commit_completion(registry, &entry) {
            Commit::Claimed => {
                info!(
                    delegation = %params.delegation_id,
                    status = ?params.status,
                    from = %entry.to_logical,
                    to = %entry.from_logical,
                    "delegation completed"
                );
                true
            }
            Commit::Vanished => {
                // Concurrent removal (duplicate result, cancel, sweep, or
                // fail_instance): whoever removed it released the capacity.
                info!(
                    delegation = %params.delegation_id,
                    namespace = %entry.namespace,
                    "entry removed concurrently after delivery — capacity already released"
                );
                false
            }
            Commit::Superseded { generation } => {
                // The id was cancelled/expired and re-admitted between peek
                // and commit. That new delegation is live and not ours to
                // touch: removing it would strand a running delegation and
                // free a slot it still occupies.
                warn!(
                    delegation = %params.delegation_id,
                    namespace = %entry.namespace,
                    peeked_generation = entry.generation,
                    live_generation = generation,
                    "delegation id re-admitted between delivery and commit — live entry left untouched"
                );
                false
            }
        };
        CompleteOutcome::Delivered { committed }
    }

    /// Handle `cp/cancel` from the initiator. Returns the frame to forward
    /// to the serving runtime, if the delegation is in flight and owned by
    /// the caller.
    ///
    /// Ownership is validated under the same lock acquisition that removes the
    /// entry (see [`Claim`] — no remove/reinsert window), and every refusal
    /// returns ONE byte-identical error: an unknown id and another instance's
    /// live id are indistinguishable to the caller, so `cp/cancel` cannot be
    /// used to probe for delegation ids. The distinction is kept in the CP's
    /// own logs only.
    pub fn cancel(
        &self,
        registry: &Registry,
        from_handle: u64,
        params: &CancelParams,
        next_rpc_id: u64,
    ) -> Result<Option<(Instance, String)>, ErrorObject> {
        let refused = || {
            ErrorObject::new(
                codes::POLICY_DENIED,
                "delegation is not in flight for this instance",
            )
        };
        let entry = match self.claim(registry, from_handle, &params.delegation_id) {
            Claim::Owned(entry) => entry,
            Claim::WrongOwner {
                namespace,
                owner_handle,
            } => {
                warn!(
                    delegation = %params.delegation_id,
                    namespace = %namespace,
                    handle = from_handle,
                    initiator = owner_handle,
                    "cancel refused: only the initiating instance may cancel"
                );
                return Err(refused());
            }
            Claim::NotFound { namespace } => {
                warn!(
                    delegation = %params.delegation_id,
                    namespace = %namespace,
                    "cancel refused: delegation not in flight"
                );
                return Err(refused());
            }
            Claim::Unregistered => {
                warn!(
                    handle = from_handle,
                    "cancel from an unregistered connection"
                );
                return Err(refused());
            }
        };
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
            let keys: Vec<DelegationKey> = g
                .iter()
                .filter(|(_, e)| e.to_handle == handle || e.from_handle == handle)
                .map(|(k, _)| k.clone())
                .collect();
            keys.iter().filter_map(|k| g.remove(k)).collect()
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
            let keys: Vec<DelegationKey> = g
                .iter()
                .filter(|(_, e)| e.deadline <= now)
                .map(|(k, _)| k.clone())
                .collect();
            keys.iter().filter_map(|k| g.remove(k)).collect()
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

    /// Chain of an in-flight delegation (for tests/inspection). Delegation
    /// ids are namespace-scoped, so the namespace is part of the lookup.
    pub fn chain_of(&self, namespace: &str, delegation_id: &str) -> Option<Vec<String>> {
        self.inflight
            .lock()
            .get(&DelegationKey::new(namespace, delegation_id))
            .map(|e| e.chain.clone())
    }

    pub fn inflight_count(&self) -> usize {
        self.inflight.lock().len()
    }
}

/// Largest index `<= max` that lands on a char boundary of `s`.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    let mut cut = max.min(s.len());
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
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
        assert_eq!(
            w.router.complete(&w.registry, w.h_worker, result, 1024, 2),
            CompleteOutcome::Delivered { committed: true }
        );
        let frame = w.primary_rx.try_recv().unwrap();
        assert!(frame.contains("\"completed\""));
        assert_eq!(w.registry.get(w.h_worker).unwrap().active_sessions, 0);
        assert_eq!(w.router.inflight_count(), 0);
    }

    #[test]
    fn inflight_exists_before_target_receives_frame() {
        // An immediately-arriving result must find the entry: it is inserted
        // before the forward frame is sent.
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
        assert_eq!(
            w.router.complete(&w.registry, w.h_worker, result, 1024, 2),
            CompleteOutcome::Delivered { committed: true }
        );
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
    fn concurrent_fail_instance_never_double_releases_capacity() {
        // delegate's rollback and fail_instance can race on the same entry:
        // fail_instance takes the in-flight lock without the admission lock,
        // so it can remove the entry (and release its reservation) between
        // delegate's insert and a failing try_send. Whoever removes the
        // entry releases the capacity — exactly once. A double release
        // silently undercounts the target (saturating math) and lets
        // `saturated()` admit work to an instance that is actually full.
        for _ in 0..200 {
            let registry = Registry::new();
            let router = Router::new();
            let cfg = cfg();
            let (p1, _p1_rx) = instance("prod", "koudu", AgentType::Primary, 4);
            let (p2, _p2_rx) = instance("prod", "koudu-2", AgentType::Primary, 4);
            let (wk, mut worker_rx) = instance("prod", "worker-1", AgentType::Worker, 4);
            let hp1 = registry.register(p1);
            let hp2 = registry.register(p2);
            let hw = registry.register(wk);

            // Baseline: a live delegation from p1 keeps the true count at 1.
            match router.delegate(
                &cfg,
                &registry,
                "prod",
                "koudu",
                &AgentType::Primary,
                hp1,
                delegate_params("d-0", "worker-1", 60),
                1,
            ) {
                DelegateOutcome::Accepted(_) => {}
                DelegateOutcome::Rejected(e) => panic!("baseline rejected: {}", e.message),
            }
            worker_rx.try_recv().unwrap();
            // Close the worker's queue so p2's forward frame is refused and
            // its delegate call takes the rollback path.
            worker_rx.close();

            let gate = std::sync::Barrier::new(2);
            std::thread::scope(|s| {
                s.spawn(|| {
                    gate.wait();
                    // p2 dies while its delegate call is in flight.
                    let mut next = || 99;
                    router.fail_instance(&registry, hp2, &mut next);
                });
                gate.wait();
                let _ = router.delegate(
                    &cfg,
                    &registry,
                    "prod",
                    "koudu-2",
                    &AgentType::Primary,
                    hp2,
                    delegate_params("d-1", "worker-1", 60),
                    2,
                );
            });

            assert_eq!(
                registry.get(hw).unwrap().active_sessions,
                1,
                "exactly the baseline delegation must stay reserved"
            );
            assert_eq!(router.inflight_count(), 1);
        }
    }

    #[test]
    fn stalled_initiator_result_is_never_silently_lost() {
        // Terminal results honor the bounded-queue contract: if the
        // initiator cannot drain its queue, the entry stays in flight and
        // the caller is told to treat the initiator as disconnected. The
        // delegation then resolves through fail_instance (cp/cancel to the
        // serving side, capacity released once) — never by silently
        // dropping a computed result while acking the serving side.
        let mut w = world();
        assert!(matches!(
            do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        w.worker_rx.try_recv().unwrap();

        // Fill the initiator's bounded queue so the result frame is refused.
        let initiator_tx = w.registry.get(w.h_primary).unwrap().tx;
        while initiator_tx.try_send("filler".into()).is_ok() {}

        assert_eq!(
            w.router
                .complete(&w.registry, w.h_worker, result_of("d-1", "late"), 1024, 2),
            CompleteOutcome::InitiatorStalled {
                initiator_handle: w.h_primary
            }
        );
        assert_eq!(w.router.inflight_count(), 1, "entry must stay in flight");
        assert_eq!(
            w.registry.get(w.h_worker).unwrap().active_sessions,
            1,
            "capacity must not be released while the delegation is unresolved"
        );

        // The stalled initiator is then failed (disconnect path): capacity
        // is released exactly once and the serving side is told to cancel.
        let mut next = || 3;
        let frames = w.router.fail_instance(&w.registry, w.h_primary, &mut next);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].1.contains("cp/cancel"));
        assert_eq!(w.router.inflight_count(), 0);
        assert_eq!(w.registry.get(w.h_worker).unwrap().active_sessions, 0);
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
    fn result_from_wrong_handle_dropped_and_entry_untouched() {
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
        assert_eq!(
            w.router.complete(&w.registry, w.h_primary, result, 1024, 2),
            CompleteOutcome::Dropped
        );
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
        assert_eq!(
            w.router.complete(&w.registry, w.h_worker, result, 1024, 2),
            CompleteOutcome::Dropped
        );
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
            result: Some("x".repeat(200)),
            error: None,
        };
        let cap = 96usize;
        assert_eq!(
            w.router.complete(&w.registry, w.h_worker, result, cap, 2),
            CompleteOutcome::Delivered { committed: true }
        );
        let frame = w.primary_rx.try_recv().unwrap();
        let v: serde_json::Value = serde_json::from_str(&frame).unwrap();
        let out = v["params"]["result"].as_str().unwrap();
        assert!(out.contains("truncated by control plane"));
        assert!(
            out.len() <= cap,
            "marker must count against the cap: {} > {}",
            out.len(),
            cap
        );

        // Degenerate tiny cap still never exceeds the cap.
        assert!(matches!(
            do_delegate(&w, delegate_params("d-2", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        w.worker_rx.try_recv().unwrap();
        let result2 = DelegateResultParams {
            delegation_id: "d-2".into(),
            status: DelegationStatus::Completed,
            result: Some("y".repeat(100)),
            error: None,
        };
        assert_eq!(
            w.router.complete(&w.registry, w.h_worker, result2, 8, 3),
            CompleteOutcome::Delivered { committed: true }
        );
        let frame2 = w.primary_rx.try_recv().unwrap();
        let v2: serde_json::Value = serde_json::from_str(&frame2).unwrap();
        assert!(v2["params"]["result"].as_str().unwrap().len() <= 8);
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
            w.router.chain_of("prod", "d-root").unwrap(),
            vec!["prod/koudu".to_string()]
        );

        // Borrowed ancestry: worker-2 (NOT serving d-root) tries to use
        // d-root as its parent — rejected, so a trusted chain and deadline
        // budget cannot be inherited by a stranger.
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
            w.router.chain_of("prod", "d-child").unwrap(),
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

    fn result_of(id: &str, body: &str) -> DelegateResultParams {
        DelegateResultParams {
            delegation_id: id.into(),
            status: DelegationStatus::Completed,
            result: Some(body.into()),
            error: None,
        }
    }

    #[test]
    fn wrong_handle_result_never_hides_the_genuine_one() {
        // Ownership is validated under the same lock acquisition that removes
        // the entry. The old remove → validate →
        // reinsert sequence made the entry briefly invisible, so a genuine
        // result arriving in that window was dropped as "unknown id".
        for spoof_first in [true, false] {
            let mut w = world();
            assert!(matches!(
                do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
                DelegateOutcome::Accepted(_)
            ));
            w.worker_rx.try_recv().unwrap();

            if spoof_first {
                // h_primary is registered but is NOT the serving instance.
                assert_eq!(
                    w.router.complete(
                        &w.registry,
                        w.h_primary,
                        result_of("d-1", "spoofed"),
                        1024,
                        2
                    ),
                    CompleteOutcome::Dropped
                );
                assert_eq!(
                    w.router.inflight_count(),
                    1,
                    "a non-owner frame must not remove the entry"
                );
            }

            assert_eq!(
                w.router.complete(
                    &w.registry,
                    w.h_worker,
                    result_of("d-1", "genuine"),
                    1024,
                    3,
                ),
                CompleteOutcome::Delivered { committed: true },
                "genuine result must be delivered, never dropped"
            );
            let frame = w.primary_rx.try_recv().unwrap();
            assert!(frame.contains("genuine"));
            assert_eq!(w.router.inflight_count(), 0);
            assert_eq!(w.registry.get(w.h_worker).unwrap().active_sessions, 0);

            if !spoof_first {
                // A late non-owner frame after completion is a plain no-op.
                assert_eq!(
                    w.router.complete(
                        &w.registry,
                        w.h_primary,
                        result_of("d-1", "spoofed"),
                        1024,
                        4
                    ),
                    CompleteOutcome::Dropped
                );
                assert_eq!(w.router.inflight_count(), 0);
            }
        }
    }

    #[test]
    fn genuine_result_survives_concurrent_non_owner_frames() {
        // The racing case the sequential test above cannot
        // observe: with remove → validate → reinsert, a genuine result that
        // lands inside the window sees an empty table and is dropped, and the
        // delegation then stalls to its deadline. Under a single lock
        // acquisition the outcome is order-independent by construction.
        for _ in 0..200 {
            let w = world();
            assert!(matches!(
                do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
                DelegateOutcome::Accepted(_)
            ));
            let gate = std::sync::Barrier::new(2);
            let (spoofed, genuine) = std::thread::scope(|s| {
                let spoof = s.spawn(|| {
                    gate.wait();
                    // Registered, but not the serving instance.
                    w.router.complete(
                        &w.registry,
                        w.h_primary,
                        result_of("d-1", "spoofed"),
                        1024,
                        2,
                    ) == CompleteOutcome::Delivered { committed: true }
                });
                gate.wait();
                let genuine = w.router.complete(
                    &w.registry,
                    w.h_worker,
                    result_of("d-1", "genuine"),
                    1024,
                    3,
                ) == CompleteOutcome::Delivered { committed: true };
                (spoof.join().unwrap(), genuine)
            });
            assert!(!spoofed, "a non-owner must never complete a delegation");
            assert!(genuine, "the genuine result must never be dropped");
            assert_eq!(w.router.inflight_count(), 0);
        }
    }

    #[test]
    fn genuine_cancel_survives_concurrent_non_owner_frames() {
        // Cancel side, racing case: with the old
        // remove → validate → reinsert pattern, a genuine initiator cancel
        // landing inside a non-owner cancel's window would see an empty table
        // and be refused, leaving the delegation to stall to its deadline.
        // Under a single lock acquisition the outcome is order-independent.
        for _ in 0..200 {
            let w = world();
            assert!(matches!(
                do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
                DelegateOutcome::Accepted(_)
            ));
            let params = CancelParams {
                delegation_id: "d-1".into(),
                reason: "race".into(),
            };
            let gate = std::sync::Barrier::new(2);
            let (spoofed, genuine) = std::thread::scope(|s| {
                let spoof = s.spawn(|| {
                    gate.wait();
                    // Registered, but not the initiator.
                    w.router.cancel(&w.registry, w.h_worker, &params, 1).is_ok()
                });
                gate.wait();
                let genuine = w
                    .router
                    .cancel(&w.registry, w.h_primary, &params, 2)
                    .is_ok();
                (spoof.join().unwrap(), genuine)
            });
            assert!(!spoofed, "a non-initiator must never cancel a delegation");
            assert!(genuine, "the genuine cancel must never be refused");
            assert_eq!(w.router.inflight_count(), 0);
            assert_eq!(
                w.registry.get(w.h_worker).unwrap().active_sessions,
                0,
                "capacity must be released exactly once"
            );
        }
    }

    #[test]
    fn refused_cancel_leaves_the_delegation_cancellable() {
        // Cancel side: a wrong-handle cancel must not
        // remove-and-reinsert the entry, and must not disturb accounting.
        let w = world();
        assert!(matches!(
            do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        let params = CancelParams {
            delegation_id: "d-1".into(),
            reason: "not mine".into(),
        };
        assert!(w
            .router
            .cancel(&w.registry, w.h_worker, &params, 1)
            .is_err());
        assert_eq!(w.router.inflight_count(), 1);
        assert_eq!(
            w.registry.get(w.h_worker).unwrap().active_sessions,
            1,
            "a refused cancel must not release capacity"
        );
        // The genuine initiator can still cancel.
        let fwd = w
            .router
            .cancel(&w.registry, w.h_primary, &params, 2)
            .unwrap()
            .unwrap();
        assert_eq!(fwd.0.handle, w.h_worker);
        assert_eq!(w.router.inflight_count(), 0);
    }

    #[test]
    fn cancel_refusals_are_byte_identical() {
        // `cp/cancel` must not be an existence oracle —
        // an unknown id and another instance's live id return the same error
        // object, byte for byte.
        let w = world();
        assert!(matches!(
            do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        let unknown = CancelParams {
            delegation_id: "d-does-not-exist".into(),
            reason: "probe".into(),
        };
        let foreign = CancelParams {
            delegation_id: "d-1".into(),
            reason: "probe".into(),
        };
        // Both probes come from the worker: it initiated neither.
        let e_unknown = w
            .router
            .cancel(&w.registry, w.h_worker, &unknown, 1)
            .unwrap_err();
        let e_foreign = w
            .router
            .cancel(&w.registry, w.h_worker, &foreign, 2)
            .unwrap_err();
        assert_eq!(
            serde_json::to_string(&e_unknown).unwrap(),
            serde_json::to_string(&e_foreign).unwrap(),
            "unknown and foreign delegation ids must be indistinguishable"
        );
        assert_eq!(e_unknown.code, codes::POLICY_DENIED);
        assert_eq!(w.router.inflight_count(), 1);
    }

    #[test]
    fn same_delegation_id_in_two_namespaces_is_independent() {
        // The in-flight table is keyed by
        // (namespace, delegation_id). A client-supplied id in one namespace
        // must neither collide with nor be observable from another.
        let registry = Registry::new();
        let router = Router::new();
        let cfg = cfg();
        let (p_prod, mut prod_init_rx) = instance("prod", "koudu", AgentType::Primary, 4);
        let (w_prod, mut prod_rx) = instance("prod", "worker-1", AgentType::Worker, 2);
        let (p_dev, mut dev_init_rx) = instance("dev", "koudu", AgentType::Primary, 4);
        let (w_dev, mut dev_rx) = instance("dev", "worker-1", AgentType::Worker, 2);
        let hp_prod = registry.register(p_prod);
        let hw_prod = registry.register(w_prod);
        let hp_dev = registry.register(p_dev);
        let hw_dev = registry.register(w_dev);

        for (ns, hp) in [("prod", hp_prod), ("dev", hp_dev)] {
            match router.delegate(
                &cfg,
                &registry,
                ns,
                "koudu",
                &AgentType::Primary,
                hp,
                delegate_params("d-1", "worker-1", 60),
                1,
            ) {
                DelegateOutcome::Accepted(ack) => {
                    assert_eq!(ack.assigned_to, format!("{ns}/worker-1"))
                }
                DelegateOutcome::Rejected(e) => {
                    panic!("{ns} rejected ({}): {}", e.code, e.message)
                }
            }
        }
        assert_eq!(
            router.inflight_count(),
            2,
            "one `d-1` per namespace, both in flight"
        );
        prod_rx.try_recv().unwrap();
        dev_rx.try_recv().unwrap();

        // A dev instance cannot cancel prod's `d-1` — and cannot learn that
        // it exists: same error as for an id that exists nowhere.
        let probe = CancelParams {
            delegation_id: "d-1".into(),
            reason: "probe".into(),
        };
        let nowhere = CancelParams {
            delegation_id: "d-nowhere".into(),
            reason: "probe".into(),
        };
        let e_cross = router
            .cancel(&registry, hw_dev, &probe, 10)
            .unwrap_err()
            .message;
        let e_nowhere = router
            .cancel(&registry, hw_dev, &nowhere, 11)
            .unwrap_err()
            .message;
        assert_eq!(e_cross, e_nowhere);
        assert_eq!(router.inflight_count(), 2);

        // Results route to the initiator of the SAME namespace only.
        assert_eq!(
            router.complete(&registry, hw_dev, result_of("d-1", "dev-done"), 1024, 12),
            CompleteOutcome::Delivered { committed: true }
        );
        let frame = dev_init_rx.try_recv().unwrap();
        assert!(frame.contains("dev-done"));
        assert!(
            prod_init_rx.try_recv().is_err(),
            "prod's initiator must not receive dev's result"
        );
        assert!(
            router.chain_of("prod", "d-1").is_some(),
            "prod's delegation must be untouched"
        );

        assert_eq!(
            router.complete(&registry, hw_prod, result_of("d-1", "prod-done"), 1024, 13),
            CompleteOutcome::Delivered { committed: true }
        );
        let frame = prod_init_rx.try_recv().unwrap();
        assert!(frame.contains("prod-done"));
        assert_eq!(router.inflight_count(), 0);
    }

    #[test]
    fn parent_lookup_is_namespace_scoped() {
        // Parent-chain resolution must not reach into
        // another namespace's in-flight table.
        let cfg: CpConfig = toml::from_str(
            r#"
[namespaces.prod]
max_depth = 5
allow_worker_initiation = true

[namespaces.dev]
max_depth = 5
allow_worker_initiation = true
"#,
        )
        .unwrap();
        let registry = Registry::new();
        let router = Router::new();
        let (p_prod, _rx1) = instance("prod", "koudu", AgentType::Primary, 4);
        let (w_prod, mut rx2) = instance("prod", "worker-1", AgentType::Worker, 2);
        let (t_prod, _rx3) = instance("prod", "worker-2", AgentType::Worker, 2);
        let (w_dev, _rx4) = instance("dev", "worker-1", AgentType::Worker, 2);
        let (t_dev, _rx5) = instance("dev", "worker-2", AgentType::Worker, 2);
        let hp_prod = registry.register(p_prod);
        let hw_prod = registry.register(w_prod);
        registry.register(t_prod);
        let hw_dev = registry.register(w_dev);
        registry.register(t_dev);

        assert!(matches!(
            router.delegate(
                &cfg,
                &registry,
                "prod",
                "koudu",
                &AgentType::Primary,
                hp_prod,
                delegate_params("d-root", "worker-1", 120),
                1,
            ),
            DelegateOutcome::Accepted(_)
        ));
        rx2.try_recv().unwrap();

        // dev/worker-1 claims prod's `d-root` as its parent: invisible.
        let mut child = delegate_params("d-child", "worker-2", 60);
        child.parent_delegation_id = Some("d-root".into());
        match router.delegate(
            &cfg,
            &registry,
            "dev",
            "worker-1",
            &AgentType::Worker,
            hw_dev,
            child,
            2,
        ) {
            DelegateOutcome::Rejected(e) => {
                assert_eq!(e.code, codes::INVALID_PARAMS);
                assert!(e.message.contains("not in flight for this instance"));
            }
            _ => panic!("cross-namespace parent must be rejected"),
        }
        // ...and the legitimate in-namespace child still works.
        let mut ok_child = delegate_params("d-child", "worker-2", 60);
        ok_child.parent_delegation_id = Some("d-root".into());
        assert!(matches!(
            router.delegate(
                &cfg,
                &registry,
                "prod",
                "worker-1",
                &AgentType::Worker,
                hw_prod,
                ok_child,
                3,
            ),
            DelegateOutcome::Accepted(_)
        ));
        assert_eq!(
            router.chain_of("prod", "d-child").unwrap(),
            vec!["prod/koudu".to_string(), "prod/worker-1".to_string()]
        );
    }

    /// Phase-1 snapshot exactly as `complete` takes it, so a test can hold a
    /// real pre-delivery entry (generation included) across an interleaved
    /// operation and then drive the commit step directly — deterministic,
    /// no barrier timing.
    fn peek(w: &World, id: &str) -> InFlight {
        match w.router.peek_for_completion("prod", id, w.h_worker) {
            Peek::Serving(e) => e,
            _ => panic!("{id} must be in flight and served by the worker"),
        }
    }

    /// How the in-flight entry is removed between peek and commit.
    #[derive(Debug, Clone, Copy)]
    enum Interleaved {
        /// The initiator cancels (`cp/cancel`).
        Cancel,
        /// The deadline sweep expires it.
        Sweep,
    }

    /// Perform the interleaved removal and return the frames it synthesized
    /// for delivery (the router builds them; the caller is what sends them).
    fn interleave(w: &World, how: Interleaved, id: &str) -> Vec<String> {
        match how {
            Interleaved::Cancel => {
                let params = CancelParams {
                    delegation_id: id.into(),
                    reason: "changed my mind".into(),
                };
                w.router
                    .cancel(&w.registry, w.h_primary, &params, 90)
                    .expect("the initiator may cancel")
                    .map(|(_, frame)| frame)
                    .into_iter()
                    .collect()
            }
            Interleaved::Sweep => {
                let mut id_seq = 900u64;
                let mut next = || {
                    id_seq += 1;
                    id_seq
                };
                let frames = w.router.sweep_deadlines(
                    &w.registry,
                    Utc::now() + Duration::seconds(3600),
                    &mut next,
                );
                assert!(!frames.is_empty(), "the sweep must have expired something");
                frames.into_iter().map(|(_, frame)| frame).collect()
            }
        }
    }

    fn drain(w: &mut World) {
        while w.worker_rx.try_recv().is_ok() {}
        while w.primary_rx.try_recv().is_ok() {}
    }

    #[test]
    fn stale_commit_never_claims_a_reused_delegation_id() {
        // The ABA case. `(namespace, delegation_id)` + serving handle is not a
        // stable identity: cancel-then-retry is an ordinary client pattern, the
        // id is client-supplied, and with a single replica the retry routes to
        // the SAME worker. A commit that matched on those alone would remove
        // the RETRY's live entry and release its capacity — the running
        // delegation becomes invisible to its own genuine result and to the
        // sweep, and its slot is handed out while still occupied.
        //
        // The generation stamp makes the commit claim the admission it
        // actually delivered for, or nothing.
        for how in [Interleaved::Cancel, Interleaved::Sweep] {
            let mut w = world(); // worker max_delegated_sessions = 1
            assert!(matches!(
                do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
                DelegateOutcome::Accepted(_)
            ));
            // A: peeked, its initiator-bound frame notionally sent.
            let a = peek(&w, "d-1");

            // A is removed and its capacity released by another path.
            interleave(&w, how, "d-1");
            assert_eq!(w.router.inflight_count(), 0, "{how:?}");
            assert_eq!(
                w.registry.get(w.h_worker).unwrap().active_sessions,
                0,
                "{how:?}: the removing path releases the capacity"
            );
            drain(&mut w);

            // B: the initiator retries the same id; the only replica is the
            // same worker, so key AND serving handle repeat exactly.
            assert!(
                matches!(
                    do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
                    DelegateOutcome::Accepted(_)
                ),
                "{how:?}: the freed slot must admit the retry"
            );
            let b = peek(&w, "d-1");
            assert_eq!(a.to_handle, b.to_handle, "{how:?}: same worker");
            assert_eq!(a.delegation_id, b.delegation_id);
            assert_ne!(
                a.generation, b.generation,
                "{how:?}: generations are never reused"
            );

            // The stale commit for A lands. It must claim nothing.
            assert_eq!(
                w.router.commit_completion(&w.registry, &a),
                Commit::Superseded {
                    generation: b.generation
                },
                "{how:?}: a stale commit must not claim the re-admitted entry"
            );
            assert_eq!(
                w.router.inflight_count(),
                1,
                "{how:?}: B must remain in flight"
            );
            assert_eq!(
                w.router.chain_of("prod", "d-1").as_deref(),
                Some(&["prod/koudu".to_string()][..]),
                "{how:?}: B must still be visible to results and to the sweep"
            );
            assert_eq!(
                w.registry.get(w.h_worker).unwrap().active_sessions,
                1,
                "{how:?}: B's capacity reservation must be intact"
            );

            // B then completes normally — its genuine result is delivered and
            // commits, releasing the capacity exactly once.
            assert_eq!(
                w.router.complete(
                    &w.registry,
                    w.h_worker,
                    result_of("d-1", "genuine"),
                    1024,
                    7
                ),
                CompleteOutcome::Delivered { committed: true },
                "{how:?}"
            );
            let frame = w.primary_rx.try_recv().expect("initiator got the result");
            assert!(frame.contains("genuine"), "{how:?}");
            assert_eq!(w.router.inflight_count(), 0, "{how:?}");
            assert_eq!(
                w.registry.get(w.h_worker).unwrap().active_sessions,
                0,
                "{how:?}"
            );
        }
    }

    #[test]
    fn commit_after_cancel_releases_capacity_exactly_once() {
        // commit-vs-cancel, driven directly: the initiator cancels between the
        // peek and the commit and no retry follows. The commit finds its
        // admission gone and must not decrement again — session counts are
        // saturating, so a double release is silent and `saturated()` would
        // then admit work to a full instance.
        let mut w = world();
        assert!(matches!(
            do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        // Second delegation on a 4-slot target, to make an underflow visible
        // as a wrong count rather than a saturating clamp at zero.
        let (extra, _extra_rx) = instance("prod", "worker-2", AgentType::Worker, 4);
        let h_extra = w.registry.register(extra);
        assert!(matches!(
            do_delegate(&w, delegate_params("d-2", "worker-2", 60)),
            DelegateOutcome::Accepted(_)
        ));
        let a = peek(&w, "d-1");

        interleave(&w, Interleaved::Cancel, "d-1");
        assert_eq!(w.registry.get(w.h_worker).unwrap().active_sessions, 0);
        drain(&mut w);

        assert_eq!(
            w.router.commit_completion(&w.registry, &a),
            Commit::Vanished
        );
        assert_eq!(
            w.registry.get(w.h_worker).unwrap().active_sessions,
            0,
            "capacity must be released exactly once, by the cancel"
        );
        // The unrelated delegation is untouched by any of this.
        assert_eq!(w.router.inflight_count(), 1);
        assert_eq!(w.registry.get(h_extra).unwrap().active_sessions, 1);
    }

    #[test]
    fn commit_after_deadline_sweep_releases_capacity_exactly_once() {
        // commit-vs-sweep, driven directly: the deadline sweep expires the
        // delegation between the peek and the commit. Same requirement as the
        // cancel case, and the initiator has already been sent the sweep's
        // `timeout` — the wire then carries two terminal frames for one id,
        // which the ADR resolves with "first terminal frame wins".
        let mut w = world();
        assert!(matches!(
            do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        let a = peek(&w, "d-1");

        // The sweep's `timeout` is the first terminal frame for this id; the
        // late `completed` result below is the second, which the initiator
        // ignores per the ADR's "first terminal frame wins".
        let swept = interleave(&w, Interleaved::Sweep, "d-1");
        assert!(
            swept.iter().any(|f| f.contains("\"timeout\"")),
            "the sweep must synthesize a timeout result for the initiator"
        );
        assert_eq!(w.router.inflight_count(), 0);
        assert_eq!(w.registry.get(w.h_worker).unwrap().active_sessions, 0);
        drain(&mut w);

        assert_eq!(
            w.router.commit_completion(&w.registry, &a),
            Commit::Vanished
        );
        assert_eq!(
            w.registry.get(w.h_worker).unwrap().active_sessions,
            0,
            "capacity must be released exactly once, by the sweep"
        );
        assert_eq!(w.router.inflight_count(), 0);
        // The freed slot is genuinely free (a double release would have made
        // the count underflow and this admission could exceed max=1).
        assert!(matches!(
            do_delegate(&w, delegate_params("d-2", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        match do_delegate(&w, delegate_params("d-3", "worker-1", 60)) {
            DelegateOutcome::Rejected(e) => assert_eq!(
                e.code,
                codes::SATURATED,
                "the worker's single slot must still bound admission"
            ),
            _ => panic!("expected SATURATED — capacity accounting drifted"),
        }
    }

    #[test]
    fn delivered_reports_whether_it_committed() {
        // Wire delivery and state commit are separate events, and the outcome
        // says which happened: an unconditional `Delivered` could not
        // distinguish "this frame ended the delegation" from "somebody else
        // already had".
        let mut w = world();
        assert!(matches!(
            do_delegate(&w, delegate_params("d-1", "worker-1", 60)),
            DelegateOutcome::Accepted(_)
        ));
        let a = peek(&w, "d-1");
        assert_eq!(
            w.router.commit_completion(&w.registry, &a),
            Commit::Claimed,
            "the live admission is claimed exactly once"
        );
        assert_eq!(
            w.router.commit_completion(&w.registry, &a),
            Commit::Vanished,
            "a repeated commit of the same admission is a no-op"
        );
        assert_eq!(w.registry.get(w.h_worker).unwrap().active_sessions, 0);
        drain(&mut w);

        // Through the public path: the entry is gone, so the frame is not
        // even delivered — a peek that finds nothing is a plain drop.
        assert_eq!(
            w.router
                .complete(&w.registry, w.h_worker, result_of("d-1", "late"), 1024, 8),
            CompleteOutcome::Dropped
        );
        assert!(w.primary_rx.try_recv().is_err());
    }

    #[test]
    fn generation_exhaustion_fails_closed_instead_of_wrapping() {
        // fetch_add on an exhausted counter would wrap and re-issue
        // generations, silently recreating the ABA the stamp prevents.
        // Unreachable in a realistic process lifetime; pinned here so a
        // refactor cannot quietly downgrade it to wrapping arithmetic.
        let w = world();
        w.router
            .next_generation
            .store(u64::MAX - 1, Ordering::Relaxed);
        // At the ceiling, fetch_update declines to store: the admission is
        // refused and the counter stays parked at MAX-1 forever.
        let out = do_delegate(&w, delegate_params("d-last", "worker-1", 60));
        assert!(
            matches!(out, DelegateOutcome::Rejected(ref e) if e.code == -32603),
            "exhausted generation space must refuse admission"
        );
        // No capacity was reserved by the refused admission.
        assert_eq!(w.registry.get(w.h_worker).unwrap().active_sessions, 0);
        // And it stays parked: the next attempt is refused too.
        let out2 = do_delegate(&w, delegate_params("d-next", "worker-1", 60));
        assert!(matches!(out2, DelegateOutcome::Rejected(ref e) if e.code == -32603));
    }
}

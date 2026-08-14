# ADR: Agent Control Plane — Direct Inter-Agent Communication

- **Status:** Proposed
- **Date:** 2026-08-06
- **Author:** chaodu-agent
- **Related:** [ACP Server with WebSocket Transport](./acp-server-websocket.md), [OAB MCP Adapter](./oab-mcp-adapter.md), [Custom Gateway](./custom-gateway.md), [Multi-Platform Adapters](./multi-platform-adapters.md)
- **Not to be confused with:** [ECS Control Plane](./ecs-control-plane.md), which is a *deployment* control plane (CRD/operator pattern for ECS). This ADR defines a *communication* control plane for agent-to-agent delegation.

---

## 1. Context & Problem

OpenAB's multi-agent collaboration today routes every bot-to-bot exchange
through a messaging platform ([docs/multi-agent.md](../multi-agent.md)):

```
Agent A ──► Discord message (@Agent B) ──► OpenAB-B ──► Agent B
Agent B ──► Discord message (@Agent A) ──► OpenAB-A ──► Agent A
```

This works, but has structural limits:

- **Platform rate limits** — Discord allows roughly 5 msg/sec per bot; a
  multi-step delegation burns the budget fast
- **Latency** — every hop is a network round trip through the platform
- **Format constraints** — 2000-char message limit, no structured payloads;
  agents exchange JSON by pasting it into chat messages
- **Noise** — orchestration traffic pollutes human-facing channels

### Prior art: Kiro CLI's in-process orchestration

Kiro CLI's `subagent` (crew) tool demonstrates the target ergonomics: a parent
agent issues one declarative tool call describing a pipeline (stages, DAG
dependencies, review loops), and an in-process orchestration layer spawns
sessions, schedules them, and returns results — via a **mandatory `summary`
tool** — without any external platform in the loop. Its lower-level
session-management primitives (`spawn_session`, `send_message` with inbox +
escalation auto-route, `interrupt`, `inject_context`, group broadcast) show
what a full control plane API eventually looks like.

Kiro's model is in-process: sessions live inside one runtime. OpenAB's
equivalent "runtime" is distributed — one OAB process per bot, spread across
ECS tasks, k3s clusters, and other substrates. The control plane therefore
needs a network registration model rather than an in-process session table.

### Existing building blocks

- **ACP-over-WebSocket server** ([ADR](./acp-server-websocket.md)) — OAB
  already exposes `/acp` (JSON-RPC over WS); deployed in production on
  multiple bots
- **Outbound-dial adapter pattern** — `gateway.rs` already dials out over WS
  to the Custom Gateway; the CP client follows the same shape
- **MCP facade precedent** — octobroker and the
  [OAB MCP Adapter](./oab-mcp-adapter.md) both put a narrow, broker-owned MCP
  tool surface in front of credentials and policy the agent must never hold

---

## 2. Decision

Introduce an **Agent Control Plane (CP)**: a hub-and-spoke registration and
routing service for direct agent-to-agent delegation, bypassing messaging
platforms entirely. Humans still see results on Discord/Slack when a primary
agent chooses to surface them; orchestration traffic never touches the
platform.

```
                          ┌────────────────────────────┐
                          │  Control Plane (openab-cp) │
                          │  • registry (who is alive) │
                          │  • router  (delegate RPC)  │
                          │  • policy  (who → whom)    │
                          └──────▲──────────────▲──────┘
                    register/WS  │              │  register/WS
                                 │              │
        ┌────────────────────────┴───┐      ┌───┴────────────────────────┐
        │ OAB runtime "koudu"        │      │ OAB runtime "worker-1"     │
        │ type=primary               │      │ type=worker (headless —    │
        │ Discord/Slack adapters     │      │ no platform adapters)      │
        │ ┌────────────────────────┐ │      │ ┌────────────────────────┐ │
        │ │ MCP facade             │ │      │ │ pool.get_or_create     │ │
        │ │ spawn_agent, ...       │ │      │ │ ACP stdio → Agent B    │ │
        │ └───────▲────────────────┘ │      │ └────────────────────────┘ │
        │   ACP stdio → Agent A      │      └────────────────────────────┘
        └────────────────────────────┘
```

Key properties:

1. **OAB runtimes dial out** to the CP and register (like CI runners
   registering with a coordinator). No inbound ingress on workers; works
   across ECS, k3s, and any substrate with outbound connectivity. Reuses the
   `gateway.rs` outbound-WS adapter pattern and the ACP-over-WS wire format.
2. **The agent-facing surface is an MCP facade** hosted by the local OAB
   runtime. Agents never hold CP credentials or addresses; they see four
   tools (§6) and nothing else.
3. **v1 is registry + router, not a DAG engine.** Orchestration logic lives
   in the primary agent's reasoning (it makes multiple delegate calls). A
   CP-side pipeline engine is a later, additive layer behind the same wire
   contract (§9).
4. **CP connectivity is strictly additive.** Loss of the CP link never
   affects normal platform (Discord/Slack) operation; the runtime reconnects
   with backoff. The CP holds no durable state, so a restart costs
   re-registration plus whatever delegations were in flight (§4).

### Naming

The config section and subsystem are named `control_plane`, not
`orchestrator`:

- Adapter config sections name the remote system they connect to (`[discord]`,
  `[gateway]`), not the local behavior. The OAB-side module is a client of
  the CP.
- v1 scope (registry, routing, policy) *is* control-plane semantics; a DAG
  engine, if added later, is one capability inside the CP, not the identity
  of the whole subsystem.
- Symmetry: the **gateway** routes human ↔ agent messages; the
  **control plane** routes agent ↔ agent messages.

---

## 3. Registration

### OAB-side config

```toml
[control_plane]
url = "wss://cp.example.internal/acp"
auth_key = "${OPENAB_CP_KEY}"          # per-agent credential, never shared
namespace = "prod"
name = "koudu"
type = "primary"                        # "primary" | "worker"
labels = { backend = "kiro", arch = "x86" }
max_delegated_sessions = 4              # backpressure signal to the CP
```

New config section ⇒ backward-compatible by default: absent section means no
CP connection, existing deployments unchanged.

### Field semantics

| Field | Meaning |
|-------|---------|
| `namespace` | **Authorization boundary.** The CP routes only within a namespace unless policy explicitly grants cross-namespace delegation. Maps to environments (prod/dev) or team fleets; gives multi-tenancy on one CP. |
| `name` | Logical agent name, unique per namespace (see replica semantics below). |
| `type` | **Policy axis**, not a tag. `primary` = user-facing (has platform adapters), may initiate delegation. `worker` = headless; serves delegations; may not initiate by default (§5). The term is `worker`, not `subagent` — a worker serves many primaries and the protocol field name should describe what it is, not one relationship it participates in. |
| `labels` | Capability metadata for label-based targeting (`{backend = "claude"}`), so primaries can request "any worker matching X" and let the CP schedule. |
| `max_delegated_sessions` | Advertised concurrency budget; CP routes around saturated workers. |

### Headless worker mode

`type = "worker"` unlocks a new deployment shape: an OAB instance with **no
platform adapters at all** — just `[agent]` + `[control_plane]`. No bot
token, no allowlists, smaller attack surface, cheaper task. Only reachable
via the CP.

### Replica semantics (rolling deploys)

ECS rolling deploys start the new task **before** the old one stops, so two
live tasks will briefly register under the same logical `name`. Registration
therefore carries a runtime-generated `instance_id`. The CP treats
same-name registrations as replicas of one logical agent and routes new
delegations to the newest healthy instance; in-flight delegations complete on
the instance that accepted them. Silent last-write-wins is explicitly
rejected.

---

## 4. Delegation Protocol

All delegation flows through the CP as JSON-RPC frames over the registered WS
connection (same transport discipline as the ACP-over-WS server).

```
Agent A ──spawn_agent (MCP)──► OAB-A ──cp/delegate──► CP ──► OAB-B ──pool──► Agent B
                                                   route by                │
                                                   name/label/ns           │
Agent A ◄──── result ◄──────── OAB-A ◄──────────── CP ◄── result frame ◄───┘
```

### Delegate frame

What the initiator sends. Note what is *absent*: there is no `chain` field.
Callers supply at most a parent reference; the CP constructs the ancestry
from authenticated identities and its own in-flight table, so a runtime cannot
forge it. This example is a *parented* delegation — a root one simply omits
both parent fields.

```json
{
  "method": "cp/delegate",
  "params": {
    "delegation_id": "d-01J...",
    "target": { "name": "worker-1" },
    "prompt": "…",
    "parent_delegation_id": "d-01H...",
    "parent_admission": 41,
    "deadline": "2026-08-06T22:45:00Z"
  }
}
```

- `target` — exact `name` or a `labels` selector (CP schedules among matches)
- `parent_delegation_id` + `parent_admission` — a **pair**, both omitted for a
  root delegation and both required together otherwise. If present, the caller
  must be the instance currently serving *that specific admission* of the
  parent, in the caller's own namespace; the CP appends to that admission's
  chain. "Currently serving that parent" means the exact forwarded admission —
  the serving handle plus the `admission` token the CP stamped on the parent's
  forwarded `cp/delegate` — **not** the serving handle plus the reusable
  `delegation_id`. The id alone names a slot that cancel-then-retry
  legitimately reuses, so handle + id would let a task from an admission that
  has already ended inherit the chain and deadline budget of whatever was
  re-admitted under the same id (see "Admissions carry a protocol-visible
  token"). An id without a token is `INVALID_PARAMS`; so is a token without an
  id, rather than being ignored as an accidental root delegation.
- `deadline` — propagated absolute deadline. A child's timeout can never
  exceed its parent's remaining budget, so orphaned workers cannot keep
  consuming tokens after the root gave up.

What the serving runtime receives is a different frame: the CP adds the
authenticated `from`, the constructed `chain`, and the `admission` token for
this admission.

```json
{
  "method": "cp/delegate",
  "params": {
    "delegation_id": "d-01J...",
    "admission": 42,
    "prompt": "…",
    "deadline": "2026-08-06T22:45:00Z",
    "from": "prod/koudu",
    "chain": ["prod/koudu", "prod/worker-2"]
  }
}
```

- `chain` — the full delegation ancestry, root first, appended at every hop.
  Enables cycle rejection (target already in chain), depth enforcement, and
  audit tracing back to the human-facing root. Every element was authenticated
  by the CP, so the serving runtime can trust it.
- `admission` — the token this runtime must echo on `cp/delegate_result` and
  match on `cp/cancel` (see "Admissions carry a protocol-visible token").

### Result delivery is protocol-mandatory

Adopting Kiro's `summary` lesson at the protocol level: a delegation is not
complete until the serving runtime returns a structured result frame
(`cp/delegate_result` with status, result text, and error detail on failure).
The serving **runtime** emits this frame when the agent's turn ends — result
delivery never depends on the sub-agent model "remembering" to report.

The frame MUST echo the `admission` token the CP stamped on the forwarded
`cp/delegate` (see "Admissions carry a protocol-visible token" below):

```json
{
  "method": "cp/delegate_result",
  "params": {
    "delegation_id": "d-01J...",
    "admission": 42,
    "status": "completed",
    "result": "…"
  }
}
```

`delegation_id` says *which delegation*; `admission` says *which admission of
it*, and the id is reusable. A frame without the token is rejected as
malformed, and one naming a superseded admission is dropped.

### Cancel frame

`cp/cancel` travels both ways — initiator → CP → serving runtime — and carries
the token in both, for the same reason results do:

```json
{
  "method": "cp/cancel",
  "params": {
    "delegation_id": "d-01J...",
    "admission": 42,
    "reason": "changed my mind"
  }
}
```

From the initiator, `admission` is the abort target: a cancel naming a
superseded admission is refused rather than removing whatever holds the id now.
CP-synthesized cancels (deadline sweep, initiator disconnect,
stalled-initiator teardown) stamp the token of the admission they are ending, so
a best-effort cancel that overtakes a same-id re-admission's forward is
identifiable at the worker as belonging to work that is already over.

### v1 contract amendments (from PR #1465 review)

The first implementation (`crates/openab-cp`) freezes the following
behaviors, resolving the review findings on identity, lifecycle, and
recovery semantics:

- **Identity binding.** CP config owns an immutable identity table: auth key
  → (`namespace`, `name`, `type`, optional capacity cap). The runtime's
  registration claims are *verified against* the key's bound identity and
  rejected on mismatch (`IDENTITY_MISMATCH`). Authorization never derives
  from self-asserted registration fields. Keys are per-agent
  (individually revocable) and presented as `Authorization: Bearer` on the
  WebSocket upgrade — never in URLs.
- **CP-constructed chain.** `cp/delegate` carries only a parent *reference*
  (`parent_delegation_id` + `parent_admission`); the CP derives the ancestry
  chain from its
  in-flight table and the authenticated caller identity, then stamps it on
  the forwarded frame. A runtime cannot forge ancestry, so depth/cycle
  checks operate on trusted data. Policy (role, depth, cycle, namespace,
  deadline caps) is enforced by the CP authoritatively; facade checks are
  defense in depth only.
- **Registration lifecycle.** The first frame on a connection MUST be
  `cp/register` (JSON-RPC 2.0 envelope validated — `jsonrpc: "2.0"` and a
  request id are required; `protocol_version` field). Registrations are
  keyed by a **CP-generated handle**, never the client-supplied
  `instance_id`: a colliding `instance_id` cannot replace or tear down
  another connection's registration, and all in-flight ownership checks
  (completion, cancellation, parent linkage) compare handles — each paired
  with the `admission` token, since the handle alone cannot say *which*
  admission of a reusable id is meant. The ack
  carries the heartbeat interval, lease window, and the effective (possibly
  clamped) concurrency budget. Instances missing heartbeats past the lease
  are deregistered, and their in-flight delegations fail immediately with a
  **side-specific** outcome: delegations the expired instance was *serving*
  send `target_disconnected` to the initiator, while delegations it had
  *initiated* send a best-effort `cp/cancel` to the still-live server (and
  release its reserved capacity). The two are mutually exclusive — one dead
  instance never produces both frames for the same delegation. Heartbeats
  refresh the lease only — CP-owned
  in-flight accounting is authoritative and never merged from runtime
  reports.
- **Registration deadline.** `cp/register` must arrive within
  `register_timeout_secs` of the completed WebSocket upgrade. WS Ping/Pong
  keeps the transport alive but does **not** extend the deadline, and a
  `cp/register` that arrives after it is never acked. On expiry the CP closes
  the socket with WS code **1008** (policy violation) and reason
  `registration timeout`. Rationale: authentication alone bounds nothing — an
  authenticated peer could otherwise park sockets indefinitely in the
  pre-registration state.
- **Registration is per-connection and first-frame-only, so lease expiry ends
  the connection.** When the CP drops a registration on its own initiative it
  also closes the socket — WS code **1008**, reason `lease expired`. Leaving
  it open would strand a connection whose every subsequent frame hits an
  absent registry entry and which can never re-register. Recovery is
  therefore always the same shape: **reconnect, re-authenticate, re-register**
  (a new handle; the old one is gone for good). Frames that arrive in the
  window between the sweep and the close are answered `NOT_REGISTERED` rather
  than dropped silently, so a client can tell a swept lease from a hung CP.
- **Per-identity connection quota.** `max_connections_per_identity` bounds
  concurrent sockets per identity and is counted **from the upgrade**, so
  pre-registration sockets occupy a slot too. An over-quota upgrade is refused
  at the HTTP layer with **503** and a body naming the quota (a bare 503 is
  indistinguishable from an overloaded CP). The slot is held by an RAII guard
  released on every exit path, so no early return, failed handshake, or panic
  can leak it. Replicas of one logical agent share one identity, so this is
  also the replica ceiling.
- **Resource bounds.** The WS transport rejects messages over
  `max_frame_bytes` before parsing; oversized `prompt`s are rejected
  (`max_prompt_bytes`); per-connection outbound queues are bounded and a
  peer that cannot drain its queue is treated as disconnected. Every outbound
  write is additionally bounded by `write_timeout_secs` and raced against the
  CP's own close signal, because a bounded queue does not bound the *writer*:
  a peer that stops reading (closed TCP receive window, half-open connection)
  would otherwise park the connection task inside one `send`, pinning that
  identity's connection quota and its in-flight delegations for as long as the
  socket survives — and lease expiry could not reclaim them, since lease
  expiry works by signalling that very task. A write that times out is a
  disconnect. Delegation admission (duplicate check → target selection →
  capacity reservation → in-flight insert) is one atomic sequence, and the
  in-flight entry exists before the forward frame is sent.
- **Terminal results are never silently dropped.** `cp/delegate_result`
  delivery commits only after the initiator's queue accepts the frame: the
  CP validates ownership, sends, and only then removes the in-flight entry
  and releases the serving instance's capacity. If the initiator's bounded
  queue refuses the frame, the serving runtime receives an error (not
  `ok: true`), the initiator is closed (WS 1008, reason
  `outbound queue overflow` — the "cannot drain → disconnected" rule applied
  to the frame where it matters most), and the delegation resolves through
  the disconnect path: `cp/cancel` to the serving runtime and exactly one
  capacity release. Best-effort frames (`cp/cancel`, sweep-synthesized
  `timeout`) remain fire-and-forget: the propagated deadline is their backstop.
- **Admissions carry a protocol-visible token; commits are exact.** Every
  admission is stamped with a CP-minted, never-reused **admission token**
  (`admission`, a per-namespace monotonic counter). `(namespace,
  delegation_id)` is deliberately not a stable identity over time — the id is
  client-supplied and cancel-then-retry is an ordinary client pattern, which
  with a single replica re-admits the same id to the same worker — so the token
  is what identifies one admission, and it travels the whole round trip:
  - the `cp/delegate` ack carries it, so the initiator can correlate;
  - the forwarded `cp/delegate` carries it, so the serving runtime learns it;
  - `cp/delegate_result` MUST echo it. It is a **required** field: a missing
    token is `INVALID_PARAMS`, never a wildcard;
  - `cp/cancel` carries it in **both** directions, also required. From the
    initiator it names the admission to abort; on every CP-synthesized cancel
    (deadline sweep, initiator disconnect, stalled-initiator teardown) the CP
    stamps the token of the admission it is ending, built from the in-flight
    entry it removed;
  - every initiator-bound terminal frame carries it, CP-synthesized `timeout`
    and `target_disconnected` included (both are built from the in-flight
    entry);
  - a **parent reference** on `cp/delegate` carries it as `parent_admission`,
    required whenever `parent_delegation_id` is present.

  The CP checks the echoed token *before* building the initiator-bound frame,
  and the commit phase removes an in-flight entry only when key, serving
  handle, AND token all match the admission it delivered a result for; anything
  else is left strictly untouched, capacity included. Without the token on the
  wire, a late result for a cancelled admission A — arriving after the same id
  was re-admitted as B to the same worker — would be delivered to the initiator
  as B's terminal frame and would then commit B (peek and commit both saw B),
  releasing capacity B still occupies and leaving B's genuine result to be
  dropped later as unknown. A stale-token result is dropped and answered with
  the same generic ack as any other drop: a distinguishable reply would tell
  the serving side whether an id is currently re-admitted, the class of oracle
  namespace-scoped keys and byte-identical `cp/cancel` refusals removed. The
  counter is per namespace for the same reason: a single global counter on the
  wire would disclose other namespaces' delegation volume, while commit
  matching only needs never-reuse per `(namespace, delegation_id)`. Exhaustion
  fails closed (the admission is refused; the counter never wraps).

  Cancellation needs the token for the same reason results do, in both
  directions. `cp/cancel` from the initiator is matched on `(from_handle,
  namespace + delegation_id, admission)` — all three under the one lock
  acquisition that removes the entry — and any mismatch is refused with the
  same byte-identical `POLICY_DENIED` as an unknown id or another instance's
  live id, so a caller cannot learn whether the id it reused is currently
  re-admitted. Without the token, an ordinary application-level *retry* of
  `cancel(A)` landing after the same id was re-admitted as B matched B and
  removed it: B's capacity was released while its work continued, and B's
  genuine result was later dropped as unknown with no synthesized terminal
  frame, because the entry was gone. In the CP-synthesized direction the gap is
  structural rather than client-dependent: the deadline sweep removes an
  expired entry under the in-flight lock and builds its best-effort
  `cp/cancel` after releasing it, so a same-id re-admission can be admitted and
  its forward enqueued to the same worker first. The worker would then receive
  `forward(B)` followed by an id-only cancel for A — different producers into
  one queue, so connection ordering does not help — and B's work would be
  aborted at the source while the CP still shows it live. Stamping the ended
  admission's token makes that frame identifiable as belonging to work that is
  already over.

  Parent linkage is the third surface, and the one that feeds the CP's own
  authorization decisions rather than frame delivery. The parent lookup exists
  to stop any runtime that knows a live id from borrowing its trusted chain and
  deadline budget, and it checks that the caller is the instance *serving* that
  parent — but "serving" resolved through `(namespace, delegation_id)` plus the
  serving handle is not an admission. Once parent admission A ends (cancel,
  completion, or sweep) and the id is re-admitted as B — with a single replica,
  to the same worker — a residual task from A submitting
  `cp/delegate { parent_delegation_id: P }` satisfies every one of those checks
  against B. The child then inherits B's CP-constructed chain and B's remaining
  deadline budget: depth, cycle, and parent-budget are evaluated for the wrong
  admission, and the audit chain attributes A's work to B's root. Unlike the
  result and cancel cases this is not misdelivery but a policy-envelope hijack,
  and the CP cannot push it onto the worker: `cp/cancel` is best effort, and the
  runtime holding the serving connection is precisely who would trigger it
  deliberately. So the reference is a pair — `parent_delegation_id` plus
  `parent_admission` — matched together with the serving handle under the one
  in-flight lock acquisition the parent branch already takes, and the chain and
  deadline are read from the entry that acquisition validated rather than from a
  second lookup. Unknown, unauthorized and stale parents share one refusal
  shape, so the reply is not an oracle for whether an id is currently
  re-admitted.

  ⚠️ **Wire-breaking (pre-1.0).** `admission` is a **required** field on both
  `cp/delegate_result` (added in the round-8 revision of this contract) and
  `cp/cancel` (added in round 9), and a **parented** `cp/delegate` must carry
  `parent_admission` alongside `parent_delegation_id` (added here). Optional
  would be a wildcard, which is exactly the
  misdelivery path the token exists to close, so there is no
  backward-compatible spelling of it. A runtime built against the pre-token
  contract will have **every** result and **every** cancel refused with
  `INVALID_PARAMS` after the CP is upgraded, and every *parented* delegation
  refused with it too. Runtimes must echo
  `DelegateForward::admission` on results, name the target admission on
  cancels, and name the parent's admission when delegating a child while
  serving that parent. Root delegations are unaffected — they carry neither
  parent field, so their wire shape is unchanged. There are no shipped clients
  at this point in the stack — every
  serving runtime learns the token from the forwarded `cp/delegate` — so the
  migration is mechanical, but it is not silent and it is not optional.
- **First terminal frame per admission wins.** More than one terminal frame may
  reach an initiator for one admission: a `completed` result can race the
  deadline sweep's synthesized `timeout`, and duplicate results are possible in
  the window between delivery and commit. **Initiators MUST treat the first
  terminal frame (`completed`, `failed`, `timeout`, `target_disconnected`) for
  a given `admission` token as authoritative and ignore every later terminal
  frame for that token.** Correlation is per admission, not per
  `delegation_id`: keyed on the reusable id, a late frame for a superseded
  admission would permanently mask the live admission's genuine terminal frame.
  The CP does not suppress the later frames: doing so would require per-id
  terminal state that a CP with no durable state deliberately does not keep.
  CP-side state is unaffected either way — the token rule above makes the
  commit exact, so exactly one path ever releases the capacity.
- **Global admission and memory bounds.** Beyond per-frame and per-connection
  limits the CP bounds its own aggregate state:
  `max_inflight_delegations` (default 4096) caps simultaneously in-flight
  delegations process-wide, enforced at admission before any capacity is
  reserved and refused with `SATURATED` — per-target
  `max_delegated_sessions` is runtime-advertised and bounds one target's
  concurrency, not the CP's memory; `max_outbound_queue_bytes` (default 16 MiB)
  caps each connection's outbound queue in **bytes** as well as entries, since
  256 queued frames of a configurable frame size is not a memory bound; and
  `default_max_delegated_sessions_cap` (default 16) clamps every advertised
  capacity that has no per-identity cap, so a runtime can never advertise its
  way out of saturation-based backpressure.
- **Teardown of a registered connection is panic-safe.** Deregistration,
  failing the connection's in-flight delegations, and downstream cancellation
  run from an RAII guard scoped to the registered lifetime, so they happen on
  the normal return path and on an unwind alike (the `expect("serializable")`
  sites on production paths make a panic reachable). Without it a panicking
  connection task left its registry entry and in-flight rows — and the capacity
  they reserve on *other* instances — for the lease sweeper to reclaim up to
  `lease_expiry_secs` later. Teardown is idempotent by construction:
  deregistration is keyed by handle and capacity is released only for in-flight
  entries actually removed, so the guard and the sweeper can both run over the
  same handle without double-releasing.
- **Capacity release follows entry removal.** Whichever path removes an
  in-flight entry (result commit, cancel, deadline sweep, instance failure,
  or a failed forward's rollback) releases its capacity reservation — and
  only that path does, exactly once. A rollback that finds its entry already
  removed by a concurrent sweep or disconnect must not decrement again:
  session counts are saturating, so a double release is silent and would
  let `saturated()` admit work to a full instance.
- **Saturation = fast-fail.** When all matching targets are at capacity the
  CP replies `SATURATED` immediately. The CP never queues — v1 has no
  durable state, and a hidden in-memory queue would contradict that.
  `NO_TARGET` (nothing matches) is a distinct error.
- **Delegation ids are scoped to `(namespace, delegation_id)`.** The id is
  client-supplied, so it is only unique within the namespace that produced
  it. Two namespaces may hold the same id concurrently, legally and
  invisibly: `DUPLICATE_DELEGATION` only ever refers to the caller's own
  namespace, parent-chain lookup never reaches across namespaces, and result
  routing resolves the id inside the sender's registered namespace. `cp/cancel`
  refusals are deliberately **indistinguishable**: an unknown id and another
  instance's live id return the same `POLICY_DENIED` error object, byte for
  byte, so cancel cannot be used as an existence oracle for other tenants'
  delegation ids. The CP's own logs keep the distinction.
- **CP restart semantics.** A CP restart is equivalent to every lease
  expiring at once *with* the connection closure that implies — except that
  the CP is not there to send it: the in-flight table and the sockets die
  together with the process, so no synthesized `timeout` or
  `target_disconnected` frame can be emitted for delegations that were in
  flight. Runtimes observe the transport drop, reconnect with backoff, and
  re-register (new handles, empty in-flight table). Initiators reconcile
  against the deadline they already propagated, which is the upper bound on
  every orphaned delegation. Once the CP is back, late
  `cp/delegate_result` frames for unknown ids are acknowledged, logged, and
  dropped so reconnecting runtimes do not error-loop, and a frame from a
  connection that has not re-registered is answered `NOT_REGISTERED`. Within a
  *live* CP the synthesized failures do happen, but per side, not both at
  once: lease expiry or disconnect sends `target_disconnected` to the
  initiator when the *serving* instance died, and a best-effort `cp/cancel`
  downstream when the *initiating* instance died. Only a deadline sweep emits
  both frames for one delegation (see below), because there both peers are
  still connected.
- **Timeout and disconnect synthesis.** A deadline sweep terminates overdue
  delegations: the initiator receives a synthesized `timeout` result and the
  serving runtime a best-effort `cp/cancel` (stop burning tokens). Worker
  disconnect → `target_disconnected` to the initiator; initiator disconnect
  → best-effort `cp/cancel` downstream.
- **Result size cap.** `cp/delegate_result.result` larger than the
  configured `max_result_bytes` (default 256 KiB) is truncated head-first
  with an explicit marker.
- **Idempotency.** `delegation_id` is the caller-generated idempotency key;
  a duplicate id already in flight **in the caller's own namespace** is
  rejected (`DUPLICATE_DELEGATION`). Only the
  instance a delegation was routed to may complete it; only the initiating
  instance may cancel it.


---

## 5. Delegation Policy

**Mechanism liberal, policy conservative.** The wire protocol supports
arbitrary delegation depth (every frame carries `chain` + `deadline`); the
default policy is strict:

| Rule (v1 default) | Value |
|-------------------|-------|
| Who may initiate | `type = "primary"` only |
| Depth | 1 (primary → worker; worker → worker denied) |
| Cycles | Always rejected (target present in `chain`) |
| Cross-namespace | Denied unless explicitly granted |

Rationale: agents are LLMs billed per token. A worker that decides "this task
is big, let me spawn three helpers," each of which decides the same, is a
cost bomb with no human in the loop — only the root primary is attached to a
channel where a human would notice. Depth-1 keeps the blast radius one hop
from a human. (Kiro enforces the same property by withholding the subagent
tool from subagents entirely.)

Relaxation is CP-side, per-namespace config — not a protocol or fleet change:

```toml
# CP-side policy
[namespace.prod.delegation]
max_depth = 2
max_descendants = 6
allow = [
  { from = "type:primary", to = "type:worker" },
  { from = "worker-refactor", to = "worker-build-*" },
]
```

---

## 6. Agent-Facing Surface: MCP Facade + CLI

Two thin frontends over one local API (Unix domain socket owned by the OAB
runtime, e.g. `/run/openab/agent.sock`). One enforcement path regardless of
caller.

### MCP facade (primary interface)

Injected per-session via ACP `session/new` `mcpServers`, so every backend
(Kiro, Claude, Codex, Gemini, …) gets the same tools with zero per-backend
integration. v1 tool surface, intentionally minimal:

| Tool | Behavior |
|------|----------|
| `spawn_agent` | Delegate a task. Blocking (waits up to deadline) or async (returns a `delegation` handle immediately). |
| `check_delegation` | Status / result by `delegation` handle. |
| `list_agents` | Registry view for the caller's namespace (names, types, labels, availability) — lets the model discover targets by label. |
| `cancel_delegation` | Cancel an in-flight delegation by `delegation` handle. |

The `delegation` handle returned by `spawn_agent` is opaque to callers and
encapsulates the `(delegation_id, admission)` pair. This extends the wire
invariant ("every surface that references an existing delegation names it by
the (id, admission) pair; a bare reusable id is never accepted as a
reference") to the local API: `check_delegation` and `cancel_delegation`
resolve the handle to the exact admission it was minted for, so a delayed
check or cancel can never observe or abort a same-id re-admission. Callers
never see or construct the two halves separately.

The facade is where policy is enforced *before* frames leave the box: schema
validation, chain/depth checks, deadline clamping, audit logging. A
prompt-injected agent can at worst make a request the facade refuses.

What the facade hides: CP credentials (stay in the runtime env, which the
agent never sees under the existing `env_clear` discipline), CP topology
(agents target names/labels, never URLs), and transport (hub today; a
different transport tomorrow would not change the tool contract).

Facade tool schemas are versioned independently of the CP wire protocol —
fleets upgrade rolling, and v1 tools must keep working while the protocol
evolves underneath.

### CLI (`openab agent <verb>`) — secondary client, same socket

- **Ops/debugging:** exec into a task and run `openab agent list` /
  `openab agent status <handle>` when a delegation hangs — the handle is the
  same opaque `(delegation_id, admission)` value `spawn` printed, so a status
  or cancel typed minutes later still names the exact admission, never a
  same-id re-admission
- **Hooks & cron:** lifecycle hooks and cron jobs can fire
  `openab agent spawn …` without new plumbing
- **Escape hatch** for backends where MCP injection proves awkward

### Explicitly deferred from v1

Kiro-style session-management primitives — inbox messaging, `interrupt`,
`inject_context`, group broadcast — arrive later behind the same socket and
facade without changing anything shipped in v1.

---

## 7. Security

Two distinct auth boundaries exist, and they must not be conflated:

1. **Runtime ↔ CP (shipped in PR 1/4):** the OAB runtime authenticates to the
   CP with `Authorization: Bearer <key>` on the WebSocket upgrade, over TCP.
   The CP binds loopback by default; any non-loopback bind requires the
   explicit `allow_insecure_bind` override and a TLS-terminating proxy (or a
   private network) in front — bearer keys must never cross untrusted
   cleartext TCP. See the "v1 contract amendments" in §4 for the enforced
   registration semantics.
2. **Agent subprocess ↔ local facade (PR 3/4, not yet shipped):** the UDS
   path is the only thing the child needs; filesystem permissions on the
   socket are the local auth boundary. The *local facade* is never exposed
   on TCP — this claim is about the UDS facade, not about the CP itself,
   which is a TCP service by design.

- **No CP credentials in the agent process.** `OPENAB_CP_KEY` lives in the
  OAB runtime env; agent subprocesses keep the existing `env_clear`
  whitelist.
- **Per-agent auth keys** to the CP (not one shared fleet key), so a single
  compromised runtime is individually revocable.
- **Per-peer identity.** Delegated prompts arrive attributed to the sending
  agent's registered `namespace/name` — unlike the current ACP-over-WS server
  which hardcodes a single `acp_client` sender id. Required for allowlisting,
  policy, and audit.
- **Namespace isolation** as the default authz boundary (§3).
- **Audit trail.** Every delegate/result frame is logged with its full
  `chain`, giving end-to-end tracing from any worker action back to the
  human-facing root.
- **Prompt-injection containment.** Policy enforcement lives in the facade
  and the CP, outside the model's reach; the agent cannot exceed granted
  scope regardless of prompt content.

---

## 8. Deployment

`openab-cp` ships as a standalone binary following the
`crates/openab-gateway` precedent (standalone companion, embeddable in a
unified build later). It shares the axum/WS scaffolding already used by the
ACP server. State is an in-memory registry rebuilt from re-registrations
after restart; durable state (persistent inboxes, delegation history) is out
of scope for v1.

---

## 9. Scope

### In scope (v1)

- `[control_plane]` config section + outbound registration client in OAB
- `openab-cp` binary: registry, router, policy engine, replica handling
- `cp/delegate` / `cp/delegate_result` wire contract with `chain` + `deadline`
- MCP facade (4 tools) over a local UDS + `openab agent <verb>` CLI
- Default policy: primary-initiated, depth 1, namespace-scoped

### Out of scope (v1) — deliberately

- **CP-side DAG/pipeline engine** (Kiro-crew-style stages/loops/fail-fast) —
  additive later behind the same wire contract; primaries orchestrate via
  multiple delegate calls in the meantime
- **Durable inboxes / offline delivery** — delegation requires both runtimes
  online; queueing is a later CP capability
- **Session-management primitives** (inbox, interrupt, inject_context,
  groups) — later, same facade
- **Worker→worker delegation** — protocol-ready, policy-denied by default

---

## 10. Alternatives Considered

| Alternative | Why not |
|-------------|---------|
| **Status quo (route via Discord/Telegram)** | Rate limits, latency, 2000-char unstructured payloads, orchestration noise in human channels. The motivating problem. |
| **Peer-to-peer mesh (each OAB dials peers' `/acp` directly)** | Works inside one VPC (Cloud Map), but requires inbound reachability on every worker, N×N config, and no central policy/audit. Falls apart across substrates (ECS + k3s + tailnet). The outbound-registration hub keeps workers ingress-free and centralizes policy. |
| **In-runtime orchestrator only (Kiro's model as-is)** | Solves intra-bot parallelism but not the actual problem: delegation *between* bots with different backends on different hosts. (Intra-bot subagent spawning via the local pool remains a natural, separate extension.) |
| **External broker with full DAG engine in v1** | Scope. Registry + router delivers the value; a pipeline engine triples the surface area and can be added behind the same contract. A thin broker project should not grow a fat brain in one step. |
| **CLI-only agent interface (no MCP facade)** | Stringly-typed, shell-quoting injection risk on arbitrary prompts, blocks the agent's shell tool for long delegations, and no structured schema to guide the model. MCP facade is primary; CLI is the ops/scripting client. |

---

## 11. Open Questions

1. ~~**Streaming intermediate output**~~ — *resolved (PR #1465 review):
   committed scope as a fast-follow behind the same wire contract. Worker
   runtimes will stream `session/update`-style chunks back through the CP.
   Rationale: streaming is the observability substrate, not a feature — it
   restores the free human visibility that Discord-mediated collaboration
   provides today. It enables a read-only observer endpoint on the CP
   (e.g. `wss://cp/.../observe?ns=prod`; separate read-only credential
   class, namespace-scoped) so a human can tail all delegation traffic
   across the fleet from one terminal. v1 ships final-result-only; the
   stream frame shape is reserved in the wire contract.
2. **CP high availability** — single instance + fast re-registration is
   acceptable for v1 (restart semantics are now defined in §4); is
   active/standby needed before multi-tenant use?
3. **Human-visibility directives** — Discord mirroring becomes a consumer
   of the delegation stream (Q1) rather than a separate mechanism; exact
   directive syntax TBD when streaming lands.
4. **AgentCore/remote runtimes** — an `agentcore-acp`-backed OAB registers
   like any other runtime; verify deadline propagation across the SDK
   boundary.

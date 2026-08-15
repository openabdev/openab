# Agent Control Plane (`openab-cp`)

Standalone control-plane service for direct agent-to-agent delegation over
WebSocket JSON-RPC, so agents delegate work to each other without
round-tripping through a chat platform. Design and wire contract:
[ADR: Agent Control Plane](adr/agent-control-plane.md).

> **Status: PR 2/4 of the control-plane stack.** PR 1/4 shipped the CP
> server binary (registry, policy, router, wire protocol); this slice adds
> the observer/lobby surface — the read-only `observer` agent type, the
> `cp/event` notification stream, and `cp/list_agents` (see
> [Observer surface](#observer-surface-lobby) below). The OAB-runtime
> client (`[control_plane]` config + registration), the MCP facade/CLI, and
> client relay land in the follow-up slices — until then nothing connects
> to this server in a stock deployment, and there is no packaged container
> image yet.

## Run

```bash
cargo run -p openab-cp -- --config cp.toml
```

Start from the annotated example config:

```bash
cp crates/openab-cp/cp.toml.example cp.toml
```

Every field is documented in the example file, including the security
rationale. The essentials:

- `listen` — defaults to loopback (`127.0.0.1:9800`). A non-loopback bind is
  refused unless `allow_insecure_bind = true` is set explicitly, and then a
  TLS-terminating proxy (`wss://`) or a private network in front is
  required: runtimes authenticate with bearer keys that must never cross
  untrusted cleartext TCP.
- `[[agents]]` — one entry per agent identity: the auth key (supports
  `${ENV_VAR}` expansion) and its immutable `namespace`/`name`/`type`
  claims. A connecting runtime must register as exactly the identity its
  key is bound to.
- Heartbeats, lease expiry, registration deadline, per-identity connection
  quotas, the outbound write timeout, and frame/prompt/result size caps are
  all configurable with safe defaults.
- Aggregate bounds keep the CP itself bounded: `max_inflight_delegations`
  (global live-admission ceiling), `max_outbound_queue_bytes` (per-connection
  outbound memory), and `default_max_delegated_sessions_cap` (clamp on
  runtime-advertised capacity for identities with no cap of their own).

## Health

`GET /health` answers `ok` (liveness only; deeper checks are tracked in
issue #1474).

## Client behavior to expect

- CP-initiated closes use WS code 1008 with a reason: `registration
  timeout`, `lease expired`, or `outbound queue overflow`. On any of these,
  reconnect, re-authenticate, and re-register.
- A peer that stops reading is disconnected: any single outbound write that
  blocks longer than `write_timeout_secs` is treated as a dead peer, so keep
  draining the socket even while busy. The same rule applies to the queue
  behind it: a connection whose outbound queue exceeds
  `max_outbound_queue_bytes` (or its entry count) is disconnected rather than
  buffered.
- **Echo the `admission` token.** The `cp/delegate` ack and the forwarded
  `cp/delegate` both carry an `admission` token identifying that one admission
  of a `delegation_id`. A serving runtime MUST copy it into the matching
  `cp/delegate_result`; the field is required, and a result naming a superseded
  admission is dropped (the ack looks the same as any other, so do not treat
  `ok: true` as proof of delivery — that is what the initiator's terminal frame
  is for).
- **Name the admission on `cp/cancel` too.** `admission` is required there as
  well, in both directions. As an initiator, send the token of the admission
  you mean to abort: a cancel naming a superseded admission is refused, which
  is what stops a retried cancel from killing the re-admission that replaced
  its target. Refusals are deliberately indistinguishable from an unknown id,
  so treat one as "not mine / not live" and reconcile against your own state
  rather than inferring anything about the CP's. As a serving runtime, match
  incoming cancels on the token: a CP-synthesized cancel carries the token of
  the admission it ends, and it can arrive *after* a forward that reused the
  same `delegation_id` — cancelling on the id alone would abort the wrong work.
- **Name the parent's admission when you delegate a child.** If you issue
  `cp/delegate` *while serving* another delegation, send
  `parent_delegation_id` **and** `parent_admission` — the token you were
  forwarded for that parent. Both or neither: an id without a token is
  refused, and so is a token without an id. "The instance currently serving
  that parent" means the specific admission you were forwarded, not your
  connection plus the parent's `delegation_id`, because that id is reusable —
  otherwise a task whose parent has already ended could inherit the chain and
  deadline budget of whatever was re-admitted under the same id. Refusals here
  use the same shape for an unknown parent, a parent you do not serve, and a
  superseded admission, so treat one as "that parent admission is over" and
  stop fanning out rather than retrying with a different token. A **root**
  delegation omits both fields and is unchanged.
- ⚠️ **Wire-breaking change (pre-1.0).** `admission` is required on
  `cp/delegate_result` and on `cp/cancel`, and `parent_admission` is required
  on any `cp/delegate` that names a parent. A runtime built against the earlier
  contract has every result, every cancel, and every parented delegation
  refused with `INVALID_PARAMS`
  after upgrading the CP; there is no compatible optional spelling, because an
  absent token would be the wildcard the field exists to remove. Root
  delegations are unaffected.
- **The first terminal frame for an `admission` token wins.** The CP delivers
  a terminal frame only from the one path that authoritatively ended the
  admission (completion commit, cancel, deadline sweep, or disconnect
  teardown), so under one CP process an initiator should see exactly one
  terminal per admission. Keep the rule anyway, as defence in depth (frames
  straddling a CP restart, future multi-instance deployments): treat the
  first terminal frame as authoritative and ignore later ones for that token.
  Correlate on `admission`, not on `delegation_id`: the id is yours to reuse
  (cancel-then-retry is legal), and a late frame for the cancelled admission
  would otherwise mask the retry's genuine result. Every terminal frame carries
  the token, including CP-synthesized `timeout` and `target_disconnected`.
- A delegation may be refused with `SATURATED` because the target is at
  capacity *or* because the CP is at `max_inflight_delegations`; an observer
  registration may be refused with the same code when its namespace is at
  `max_observers_per_namespace`. The error message always says which bound
  was hit. The CP never queues — retry later (or raise the named knob).
- The capacity a runtime advertises in `max_delegated_sessions` is clamped by
  the CP (`default_max_delegated_sessions_cap`, or a per-identity override).
  The ack's `effective_max_delegated_sessions` is the value that counts.
- After a lease expires or the CP restarts, in-flight delegations are gone:
  initiators reconcile against their own deadlines and re-delegate.

## Observer surface (lobby)

A third identity type joins `primary`/`worker`: **`observer`** — a read-only
lobby client, authenticated by the same per-key identity binding
(`type = "observer"` on the `[[agents]]` entry). Observers register via the
same `cp/register` first-frame rule and hold a lease like any agent, but they
are read-only by construction: never selectable as a delegation target, and
unconditionally refused as an initiator — at the policy layer, at target
selection, and by an up-front method guard. There is no configuration that
relaxes this.

### `cp/event` notifications

The CP pushes JSON-RPC **notifications** (method `cp/event`, no `id`) to
every observer in the event's namespace. Envelope:

```json
{"jsonrpc":"2.0","method":"cp/event","params":{
  "seq": 7, "ts": "2026-08-14T20:00:00Z", "namespace": "prod",
  "event": "delegation_requested", "...": "event-specific fields"
}}
```

Event kinds and their fields:

| `event` | Fields |
|---------|--------|
| `agent_registered` | `agent`, `type`, `instance_id`, `labels` |
| `agent_deregistered` | `agent`, `instance_id`, `reason` (`disconnect` / `lease_expired`) |
| `delegation_requested` | `delegation_id`, `admission`, `from`, `to`, `prompt_excerpt`?, `deadline`, `chain` |
| `delegation_completed` | `delegation_id`, `admission`, `from`, `to`, `status`, `result_excerpt`?, `error`? |
| `delegation_cancelled` | `delegation_id`, `admission`, `from`, `to`, `by`, `reason`? |

Client contract:

- **Sequence numbers.** `seq` is per-namespace, monotonic, and dense: your
  first received frame sets your baseline, and a gap means frames were
  dropped for you (saturated queue) — resync your roster via
  `cp/list_agents`. A `seq` regression means the CP restarted: treat it as a
  full resync. `seq` is not durable across restarts.
- **Correlate on `(namespace, delegation_id, admission)`.** A delegation id
  is legally reusable (cancel-then-retry); the `admission` token is what ties
  a terminal event to the exact admission it ends, mirroring the wire frames.
- **Lifecycle ordering.** `delegation_requested` is published before the
  forward reaches the worker, and a terminal event is published only by the
  path that authoritatively ended the admission — so you never see a terminal
  before its `requested`, never more than one terminal per admission, and the
  terminal you see is the outcome the CP committed. One edge to know: if the
  initiator's connection cannot accept its terminal frame (queue refused, or
  it died first), the CP disconnects it and the frame is not delivered — the
  observer still sees the committed outcome, which that initiator never
  received. An `agent_deregistered` without a matching `agent_registered` is
  possible (registration ack failure); treat removal of an unknown agent as
  a no-op.
- **Terminal asymmetry.** Completion-shaped endings (`completed`, `failed`,
  `timeout`, `target_disconnected`) arrive as `delegation_completed` with a
  `status`; cancellations (initiator cancel, initiator disconnect) arrive as
  `delegation_cancelled` with `by` — the initiator's logical id, or the
  literal `"control-plane"` for CP-synthesized cancellations.
- **Best-effort delivery.** Fan-out uses the same bounded per-connection
  queue as everything else: a lobby that cannot keep up loses frames and
  detects it via the `seq` gap. Sends are non-blocking, so a single slow
  observer cannot block a delegation — but fan-out serialization runs inside
  the delegation path's in-flight critical section, so the observer
  population adds bounded latency to delegation bookkeeping. The bound is
  configuration, not hope: `max_observers_per_namespace` (default 16) caps
  the population and `max_event_excerpt_bytes` (validated to at most 64 KiB)
  caps the per-frame work.
- **Content redaction.** Prompt/result excerpts are bounded by
  `max_event_excerpt_bytes` (default 4 KiB). In a `metadata_only = true`
  namespace, agent-supplied content (prompt/result excerpts, worker-reported
  error text, initiator cancel reasons) is omitted entirely, while
  CP-synthesized diagnostics (timeout/disconnect reasons) remain — the stream
  stays metadata-complete but content-free.

### `cp/list_agents`

Any registered client (observers included) may call `cp/list_agents` (empty
params). It returns the roster of the **caller's own namespace** — name,
type, `instance_id`, labels, and load (`active_sessions` /
`max_delegated_sessions`) per instance; the scope comes from the
authenticated registration, never from the frame. This is the lobby's roster
view and the resync path after a `seq` gap. Note the recovery scope: the
snapshot restores the roster, not missed delegation lifecycle events — a
lobby that needs delegation history must retain its own.

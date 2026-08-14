# Agent Control Plane (`openab-cp`)

Standalone control-plane service for direct agent-to-agent delegation over
WebSocket JSON-RPC, so agents delegate work to each other without
round-tripping through a chat platform. Design and wire contract:
[ADR: Agent Control Plane](adr/agent-control-plane.md).

> **Status: PR 1/4 of the control-plane stack.** This slice ships the CP
> server binary (registry, policy, router, wire protocol). The OAB-runtime
> client (`[control_plane]` config + registration), the MCP facade/CLI, and
> streaming land in the follow-up slices — until then nothing connects to
> this server in a stock deployment, and there is no packaged container
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
- **The first terminal frame for an `admission` token wins.** A `completed`
  result can race the CP's synthesized `timeout`, so an initiator may receive
  more than one terminal frame for the same admission. Treat the first as
  authoritative and ignore later ones; the CP does not suppress them.
  Correlate on `admission`, not on `delegation_id`: the id is yours to reuse
  (cancel-then-retry is legal), and a late frame for the cancelled admission
  would otherwise mask the retry's genuine result. Every terminal frame carries
  the token, including CP-synthesized `timeout` and `target_disconnected`.
- A delegation may be refused with `SATURATED` because the target is at
  capacity *or* because the CP is at `max_inflight_delegations`; the error
  message says which. The CP never queues — retry later.
- The capacity a runtime advertises in `max_delegated_sessions` is clamped by
  the CP (`default_max_delegated_sessions_cap`, or a per-identity override).
  The ack's `effective_max_delegated_sessions` is the value that counts.
- After a lease expires or the CP restarts, in-flight delegations are gone:
  initiators reconcile against their own deadlines and re-delegate.

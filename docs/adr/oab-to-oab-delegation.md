# ADR: Authenticated OAB-to-OAB Delegation with Capability Attenuation and Signed Provenance

- **Status:** Proposed
- **Date:** 2026-07-22
- **Author:** @chaodu-agent
- **Discussion:** [Discord delegation discussion](https://discord.com/channels/1490282656913559673/1529261985881919529)

## 1. Context

OpenAB currently runs an agent behind a platform adapter or an ACP connection. A
running OAB agent can therefore answer messages and invoke its locally configured
agent runtime, but there is no first-class, authenticated way for one OAB agent
to ask another trusted OAB agent to perform a bounded subtask.

Forwarding a prompt through a chat platform is not an adequate delegation
mechanism. It loses the execution identity, makes capability boundaries
implicit, complicates cancellation and retry, and makes it difficult to prove
which agent produced an artifact or side effect. Reusing the caller's sessions,
credentials, environment, or live tool handles would also turn delegation into
authority cloning rather than authority attenuation.

The desired model is a parent OAB agent delegating a narrowly scoped task to a
worker OAB agent while preserving:

- authenticated peer identity;
- least-authority capability attenuation;
- isolated execution state;
- explicit lifecycle and failure semantics; and
- tamper-evident provenance from the original request to the result.

This ADR defines the protocol and security contract. It does not implement the
adapter or change the behavior of existing platform adapters.

## 2. Decision

Introduce a dedicated OAB delegation control plane with two roles:

- **`DelegationClient`** — sends authenticated, bounded requests from a parent
  OAB agent.
- **`DelegationServer`** — authenticates the caller, authorizes the request,
  executes it through a locally provisioned agent session, and returns a signed
  result envelope.

A transport-specific **delegation adapter** may implement the control-plane
boundary, but it is not a `ChatAdapter` and must not route delegation through
Discord, Slack, or another user-facing chat surface.

The MVP is an inter-OAB protocol: OAB A connects to a preconfigured remote OAB B
through authenticated HTTPS for bootstrap/health and a long-lived WebSocket
carrying JSON-RPC messages. The WebSocket is only a transport; delegation never
travels as a Discord, Slack, or webhook message. Remote federation and dynamic
peer discovery are deferred, but the first implementation is already remote
OAB-to-OAB execution rather than an in-process subagent API.

Where execution is backed by an ACP agent, the server reuses OpenAB's existing
ACP connection and session-pool lifecycle: bounded prompts, cancellation,
process-group cleanup, and controlled child environments remain local to the
callee.

## 3. Authority and identity model

A delegation request is an authority reduction, never an authority transfer.
The callee computes the effective authority as the intersection of four
independent constraints:

```text
effective_authority =
    caller_grants
  ∩ callee_policy
  ∩ request_capabilities
  ∩ deployment_limits
```

An empty intersection is a denial. The caller cannot grant a capability it does
not possess, and the request cannot widen the callee's policy or deployment
limits.

### 3.1 Peer identity

"Trusted OAB agent" means an authenticated agent identity, not a Discord UID,
bot display name, branch name, or message origin. The server maintains an
explicit deny-by-default peer allowlist containing an agent ID and a public-key
identity (or a configured public-key identity bound to the remote OAB endpoint).

The principal chain is preserved separately from peer authentication:

```text
human or system principal → parent OAB agent → callee OAB agent
```

A callee may authorize the parent peer while still recording the initiating
principal. No agent may impersonate a human or another peer in the chain.

### 3.2 Capability attenuation

Capabilities are structured, namespaced values such as:

```text
repo:read
repo:test
artifact:write
network:read:approved-hosts
```

The MVP deliberately omits `delegate` from the capability catalogue. Every
request is evaluated against the four-way authority intersection before an
execution session is created. Capability names, resource scopes, expiry, and
budgets are part of the request digest.

The server must reject unknown or malformed capabilities rather than silently
ignoring them. A future capability registry may add resource-specific limits,
but it must preserve monotonic attenuation.

## 4. Wire contract

The wire protocol is JSON-RPC-shaped and version-negotiated. The initial method
set is:

```text
delegate/describe
delegate/task.create
delegate/task.get
delegate/task.events
delegate/task.send
delegate/task.interrupt
```

`delegate/task.create` accepts a normalized request with at least:

```json
{
  "protocol_version": 1,
  "request_id": "parent-request-123",
  "requested_by": "agent-a",
  "principal_chain": ["principal-x", "agent-a"],
  "task": "Review this diff for security issues",
  "capabilities": ["repo:read", "repo:test"],
  "workspace": "isolated-worktree-ref",
  "deadline": "2026-07-22T05:00:00Z",
  "budget": {"max_runtime_ms": 300000, "max_output_bytes": 200000},
  "delegation_depth": 0,
  "nonce": "unique-request-nonce",
  "idempotency_key": "parent-task-123"
}
```

The request schema MUST NOT contain credentials, session handles, raw
environment maps, or live tool handles. Workspace references identify a
callee-approved resource; they are not a mechanism for passing an arbitrary
host path.

The server emits explicit lifecycle states:

```text
received → validated → reserved+audited → executing → terminal
```

Terminal states are `completed`, `failed`, `cancelled`, `expired`, and `unknown`.
Requests that cannot be parsed, whose protocol version is unsupported, or whose
signature cannot be verified are rejected immediately:

```text
received → rejected+audited
```

A structurally valid and authenticated request can be rejected after policy
validation:

```text
received → validated → rejected+audited
```

A request must never reach `executing` without a committed reservation and audit
record. OAB A may wait on the same task, reconnect, or resume event delivery
without creating a second task.

### 4.1 Canonicalization and digests

Protocol v1 uses deterministic canonical serialization before hashing. The
implementation should use RFC 8785 JSON Canonicalization Scheme (JCS), or a
protocol-compatible deterministic encoding if the implementation cannot support
JCS. The chosen encoding is part of protocol-version negotiation; ordinary JSON
object key order is not sufficient.

- `request_digest` covers the complete normalized request, including authority-
  relevant fields, capabilities, workspace, deadline, budget, nonce, and
  protocol version.
- `authority_digest` covers the evaluated authority inputs and resulting
  effective capability set.
- `result_digest` covers the exact result envelope payload before its signature.

## 4.2 Task and event identity

The remote task is not identified by a mutable display name or routing path:

- `request_id` is supplied by OAB A as the idempotency key for task creation.
- `task_id` is generated by OAB B and is immutable and globally unique. It is
  the storage, correlation, and crash-recovery key.
- `task_path` is a human-readable, mutable routing/ancestry path. Rename or
  reparent operations update path mapping only; they never rewrite event
  ownership.
- `turn_id` identifies each execution or follow-up turn under one task.
- An event is identified by `(task_id, turn_id, sequence)`.
- `cursor` is a separate durable, monotonically increasing replay position.

All follow-up, wait, interrupt, and reconnect operations first resolve any
human-readable path to `task_id`, then use the immutable ID. A duplicate
`request_id` may return the existing `task_id` and result, but may not create a
second remote task.

## 5. Signed result and provenance envelope

The callee returns a result envelope that is immutable once signed:

```json
{
  "protocol_version": 1,
  "request_id": "parent-request-123",
  "task_id": "callee-task-456",
  "turn_id": "turn-1",
  "requested_by": "agent-a",
  "principal_chain": ["principal-x", "agent-a", "agent-b"],
  "executed_by": "agent-b",
  "effective_capabilities": ["repo:read"],
  "authority_digest": "sha256:...",
  "request_digest": "sha256:...",
  "result_digest": "sha256:...",
  "outcome_digest": "sha256:...",
  "status": "completed",
  "side_effects": [],
  "artifacts": [
    {
      "uri": "artifact://agent-b/sha256:...",
      "owner": "agent-b",
      "created_by": "agent-b",
      "digest": "sha256:...",
      "derived_from": []
    }
  ],
  "issued_at": "2026-07-22T04:50:00Z",
  "expires_at": "2026-07-22T05:50:00Z",
  "nonce": "unique-request-nonce",
  "callee_key_id": "agent-b-key-1",
  "attestation": {
    "algorithm": "ed25519",
    "signature": "base64url..."
  }
}
```

The callee signature binds the request to the result:

```text
signature = Sign_callee(
    request_digest
  + result_digest
  + authority_digest
  + principal_chain
  + task_id
  + turn_id
  + issued_at
  + expires_at
  + nonce
)
```

The exact signed payload is the canonical serialized form, not an informal
concatenation. A caller may summarize a result for its user or create a new
derived artifact, but it may not rewrite `executed_by`, effective capabilities,
side effects, artifact ownership, or the principal chain.

An artifact created by the callee remains callee-owned. A caller-created derived
artifact receives a new immutable owner and a `derived_from` provenance edge;
ownership cannot be overwritten or transferred by forwarding the result.

Mutation records contain at least:

```text
target, action, executor_identity, timestamp, outcome, before_digest, after_digest
```

## 6. Security invariants

The following are protocol invariants and must be enforced fail-closed by the
server and result validator:

1. **Recursive delegation is prohibited in the MVP.**
   `delegation_depth > 0` is rejected with stable code
   `recursive_delegation_forbidden`. The request capability catalogue does not
   expose `delegate`.
2. **Credential forwarding is prohibited.** Any credential, session, raw
   environment, or live tool-handle field is rejected with
   `credential_forwarding_forbidden`. The callee uses only its locally
   provisioned identity.
3. **Artifact ownership is immutable.** `artifact.owner != artifact.created_by`
   for a newly created artifact is rejected with
   `artifact_ownership_mismatch`, except where a new derived artifact and an
   explicit `derived_from` edge are present.
4. **Capability widening is prohibited.** The effective capability set must be
   a subset of every input authority set and deployment limit.
5. **No secret inheritance.** Child execution receives no parent credentials,
   session state, environment variables, or live tool objects. The existing
   OAB child-process environment allowlist remains the default.
6. **No chat transport impersonation.** A chat message or bot UID is not a
   delegation credential and cannot select the internal delegation bypass.
7. **Signed provenance is mandatory.** A result without a valid callee signature,
   matching request digest, matching result digest, and intact principal chain
   is not accepted as a delegated result.
8. **Replay is rejected before execution.** A reused nonce, expired envelope,
   duplicate idempotency key with a conflicting request digest, or unsupported
   protocol version is rejected and audited.

## 7. Atomic reservation, audit, and replay protection

Nonce/idempotency reservation and durable audit recording must commit together,
using one database transaction or a transactional outbox. The only valid
execution transition is:

```text
validate request
  → atomically reserve nonce/idempotency key
  → durably record audit event (or commit transactional outbox)
  → commit
  → start execution
```

If reservation, audit persistence, or the outbox commit fails, the server returns
`reservation_failed` or `audit_unavailable` and does not create a child session,
call a tool, or create an artifact.

Audit events record at least the delegation ID, request digest, peer identity,
principal chain, effective capabilities, decision, rejection code when present,
and timestamp. The audit event is durable before execution begins.

A retry with the same `request_id` and the same request digest returns the
existing `task_id`, existing terminal result, or resumes the one incomplete
execution. A conflicting digest is rejected. A terminal record persists the
exact signed envelope and its `outcome_digest`. Repeating a request for the same
`task_id` can therefore only return the existing outcome or recover the unfinished
execution; it must never start a second execution.

Rejection tests must assert all of the following:

- stable rejection code;
- no child session, tool call, artifact, or other execution side effect;
- durable audit event containing the rejection reason; and
- no second execution when the request is retried.

## 8. Lifecycle, progress, and failure semantics

`delegate/task.create` returns the caller's `request_id` together with the
callee-generated immutable `task_id` and current state. OAB A may wait through
`delegate/task.events` or query `delegate/task.get`. Progress events carry a
monotonically increasing sequence number and durable cursor so a disconnected
caller can reconnect without resubmitting the task. `delegate/task.send` creates
a new `turn_id` under the same task; `delegate/task.interrupt` propagates
cancellation.

Cancellation is best effort: the server records `cancel_requested`, asks the
local ACP session to cancel, and emits a terminal `cancelled` result only after
local execution stops. A timeout becomes `expired`; it must not be reported as
successful merely because the transport disconnected.

The parent and worker use a lease/heartbeat on the remote connection. A
disconnect does not automatically cancel, duplicate, or claim success for the
task. OAB A must reattach using the immutable `task_id` and last durable cursor;
if OAB B cannot prove the outcome, it returns `unknown` and OAB A must not blindly
retry.

The protocol must distinguish:

- `failed`: execution returned a known error;
- `cancelled`: cancellation was observed;
- `expired`: deadline or lease expired before completion; and
- `unknown`: the worker cannot prove whether external side effects occurred.

Every terminal state persists an `outcome_digest` and the corresponding signed
result envelope. No status other than `completed` may be presented as a successful
delegated result. A reconnect or retry with the same immutable `task_id` returns
that persisted outcome or resumes the single incomplete execution; it never
re-executes a terminal task.

## 9. MVP scope and non-goals

### In scope

- Inter-OAB remote execution over authenticated HTTPS/WebSocket transport.
- Preconfigured remote OAB peers with authenticated identity and deny-by-default
  allowlist.
- One parent-to-one worker delegation request at a time, with bounded runtime,
  output, and capability scope.
- Text task input and streamed progress.
- Isolated worker session using existing ACP/session-pool lifecycle.
- Capability attenuation, signed result envelopes, artifact digests, and audit
  records.
- Cancellation, deadline, idempotency, replay protection, and explicit terminal
  states.
- Negative tests for schema, environment, authorization, result validation, and
  replay paths.

### Explicitly prohibited or deferred

- Recursive delegation and nested worker trees.
- Credential, session, environment, or live tool-handle forwarding.
- Caller-owned attribution of callee-created artifacts.
- Dynamic peer discovery, cross-organization federation, and automatic trust
  enrollment.
- Delegation through Discord, Slack, webhooks, or other public chat channels.
- Shared conversation history or implicit parent-session access.
- Arbitrary tool grants, unrestricted host paths, and unbounded network access.
- Durable execution semantics that survive worker loss without a separate
  scheduler/workflow design.

Federation may later add trusted nodes and provenance edges, but it must not
relax the three core rejection rules: recursive delegation, credential
forwarding, and ownership mismatch.

## 10. Negative-test acceptance criteria

The implementation PR for this ADR must add tests that independently exercise
all four enforcement layers:

### Schema layer

- Reject credential/session/environment/live-handle fields.
- Reject unknown or malformed capabilities.
- Reject unsupported protocol versions and malformed canonical payloads.

### Execution layer

- Verify the child environment contains only the deployment allowlist and
  explicitly provisioned local values.
- Verify no parent session or live tool handle is visible to the worker.
- Verify an unauthorized workspace reference cannot escape the worker boundary.

### Authorization layer

- Reject an unknown peer identity.
- Reject `delegation_depth > 0` with
  `recursive_delegation_forbidden`.
- Reject capability widening when requested capabilities exceed any authority
  intersection input.
- Verify the effective capability set is recorded in the authority digest.

### Result and provenance layer

- Reject a tampered request digest or result digest.
- Reject a missing or invalid callee signature.
- Reject `owner != executor` with `artifact_ownership_mismatch`.
- Accept a valid derived artifact only when it has a new owner and a valid
  `derived_from` edge.
- Verify mutation side effects contain target, operation, executor, timestamp,
  and outcome.

### Wire-level replay and atomicity

- Reject a reused nonce with `replay_detected`.
- Reject an expired request with `request_expired`.
- Reject a conflicting reuse of an idempotency key.
- Verify reservation and audit are atomic: if either persistence step fails,
  execution does not start.
- Verify every rejection produces a durable audit event and no execution side
  effect.
- Verify a retry of an accepted request does not create a second execution or
  second successful result.

## 11. Alternatives considered

### Forward a prompt through a chat platform

Rejected. Chat transport does not provide peer authentication, capability
attenuation, lifecycle control, replay protection, or signed provenance. It also
creates bot-loop and attribution problems.

### Share the caller's ACP session with the callee

Rejected. Session sharing crosses conversation state and authority boundaries,
prevents independent cancellation and auditing, and makes it impossible to
prove which agent performed a tool call.

### Pass the caller's credentials or environment to the callee

Rejected. This is credential forwarding and authority cloning. The callee must
use its own locally provisioned identity.

### Let the callee delegate recursively

Rejected for the MVP. Recursive trees multiply cost and complicate provenance,
lease ownership, replay protection, and failure recovery. A future orchestrator
mode can be designed as a separate protocol revision with explicit depth and
budget controls.

### Use an unauthenticated HTTP endpoint with a shared static token

Rejected for remote federation. A static token does not provide strong peer
identity, rotation, request signing, or useful provenance. It may be acceptable
as a narrowly scoped local development fixture, but not as the production wire
contract.

## 12. Prior art

### OpenClaw

OpenClaw documents a multi-agent Gateway model in [Multi-agent
routing](https://github.com/openclaw/openclaw/blob/main/docs/concepts/multi-agent.md).
Each agent has separate workspace, state directory, auth profiles, and session
store, and routing uses explicit bindings. OpenClaw also makes agent-to-agent
messaging opt-in and allowlisted, and per-agent tool policies can only further
restrict global policy.

We adopt the separation of agent state, explicit routing, and monotonic
per-agent restrictions. OpenClaw's model is primarily co-located routing inside
one Gateway process; it does not define the cross-process OAB wire contract
needed here, particularly the request/result digest binding and callee-signed
artifact provenance.

### Hermes Agent

Hermes documents a `delegate_task` tool in [Subagent
Delegation](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/features/delegation.md).
It creates fresh child conversations and terminal sessions, limits concurrent
children, supports cancellation and background completion, and blocks recursive
delegation for leaf children by default. Hermes also records durable completion
records for background results and treats an interrupted child with unknown
side effects as `unknown` rather than claiming success.

We adopt the fresh-context boundary, bounded concurrency/lifecycle, explicit
unknown outcome, and default-flat delegation. We diverge by making the OAB
boundary an authenticated peer protocol with capability intersection, no
credential forwarding, atomic replay reservation, and a callee signature that
binds the request digest to the result digest and authority digest.

## 13. Consequences

### Positive

- A parent can use a running OAB worker without treating it as a chat user.
- Authority is narrowed at every boundary and cannot be widened by the caller.
- Credentials and session state remain local to the executing agent.
- Results and artifacts can be independently verified and audited.
- The protocol supports a remote OAB user story while leaving discovery and
  cross-organization federation for later, without changing the provenance model.

### Costs and residual risks

- Key management, canonicalization, nonce storage, and durable audit increase
  implementation complexity compared with forwarding a prompt.
- A worker crash during an external side effect may remain `unknown`; the client
  must reconcile status rather than retry blindly.
- The MVP requires preconfigured remote endpoints and key rotation remains an
  operational responsibility until federation is designed.
- Artifact storage and garbage collection are not defined by this ADR; the
  implementation must provide stable content-addressed references or defer
  artifact-producing tasks.
- The ADR defines a contract, not a cryptographic implementation review. The
  implementation PR must pin algorithms, key rotation behavior, and library
  choices before enabling remote federation.

## 14. Implementation sequence

1. Add protocol types, canonical digest helpers, stable error codes, and negative
   schema tests.
2. Add HTTPS/WebSocket transport, remote peer authentication, and the deny-by-
   default delegation policy.
3. Add atomic nonce/idempotency reservation plus audit/outbox persistence.
4. Add the worker execution bridge over the existing ACP/session-pool lifecycle,
   with local environment and workspace restrictions.
5. Add signed result validation, artifact provenance, progress, cancellation,
   and crash reconciliation.
6. Add end-to-end tests for the state machine and all negative-test acceptance
   criteria in §10.
7. Consider remote mTLS federation only after the local protocol is stable and
   its signed-envelope fixtures are independently verifiable.

## 15. References

- [OpenAB ACP connection and session pool](../../crates/openab-core/src/acp/connection.rs)
- [OpenAB child-process environment policy](../../AGENTS.md#3-security--child-process-environment)
- [OpenClaw multi-agent routing](https://github.com/openclaw/openclaw/blob/main/docs/concepts/multi-agent.md)
- [Hermes Agent subagent delegation](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/features/delegation.md)
- [RFC 8785 — JSON Canonicalization Scheme](https://www.rfc-editor.org/rfc/rfc8785)
- [Ed25519 signatures](https://www.rfc-editor.org/rfc/rfc8032)

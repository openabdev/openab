# ADR: OAB Delegation Adapter for Inter-OAB Remote Tasks

- **Status:** Proposed
- **Date:** 2026-07-22
- **Author:** @chaodu-agent
- **Discussion:** [Discord delegation discussion](https://discord.com/channels/1490282656913559673/1529261985881919529)

## 1. User story

A running OAB agent should be able to delegate work instead of doing the work
itself:

```text
Agent A
  -> OAB A delegation adapter
  -> authenticated remote OAB B
  -> Agent B performs the task
  -> OAB A waits for the result
  -> Agent A continues its original task
```

For example, while reviewing a change, Agent A may ask a remote OAB specialized
in security review to inspect the diff. Agent A should receive progress and a
structured final result, then use that result in its own conversation. The
remote worker is an OAB peer, not a Discord user, a forwarded chat message, or
an exposed ACP process.

This is an **inter-OAB operation**. The adapter must work when OAB A and OAB B
run in different processes, hosts, or clusters. A relay may be used when both
OAB instances can only make outbound connections, but a particular relay
provider is not part of this protocol decision.

## 2. Decision

Add a dedicated **delegation adapter** with a client side in OAB A and a server
side in OAB B. The decision intent is to use **A2A 1.0 as the inter-OAB wire
candidate**, with an OAB Delegation Profile that carries the OAB-specific
security and reliability contract:

```text
Agent A
  |
  | local delegate operation
  v
OAB A DelegationAdapter / A2A Client
  |
  | A2A standard HTTP+JSON or JSON-RPC over HTTP + SSE
  | optional A2A custom WSS binding
  v
OAB B DelegationAdapter / A2A Server
  |
  | OAB Delegation Profile enforcement
  | local session and policy enforcement
  v
Agent B via ACP
```

The adapter is not another `ChatAdapter`. Chat adapters translate human-facing
platform events; the delegation adapter translates a bounded local agent
operation into an A2A task and maps the remote lifecycle back to the caller.

OAB B keeps its ACP connection, credentials, environment, workspace policy, and
tool approvals local. Neither A2A nor the OAB profile may expose ACP directly to
a remote peer or forward OAB A's session, credentials, environment, or live
tool handles.

This decision has a gate before a binding becomes normative. Until the gate
passes, the adapter and coordinator remain transport-neutral and no WSS,
SSE, or `delegate/*` method names are a compatibility promise. If the gate
fails, OAB falls back to a custom OAB protocol rather than shipping an A2A
wrapper that changes A2A core semantics or falsely claims standard
interoperability.

### 2.1 Transport and protocol boundary

The preferred standard binding is authenticated HTTP+JSON or JSON-RPC over HTTP
with SSE for server-to-client progress and artifact updates. Ordinary HTTP
operations carry task creation, state retrieval, and cancellation. The A2A
operation mapping is:

```text
remote task creation  -> SendMessage(returnImmediately: true)
task state/result     -> GetTask
task progress/events  -> SendStreamingMessage or SubscribeToTask
task cancellation     -> CancelTask
peer description      -> AgentCard / GetExtendedAgentCard
```

A persistent WSS transport remains possible only as an A2A custom binding. Such
a binding must preserve the A2A core data model and operation semantics, declare
itself in the Agent Card, and document event ordering, reconnect, termination,
authentication, and error mapping. It must not silently become a second
`delegate/*` protocol.

A deployment may connect directly using the selected binding, or use a relay
when both OAB instances are outbound-only. The spike must compare the standard
HTTP/SSE topology with the existing outbound WSS pairing:

```text
OAB A -- outbound selected binding --> OAB B

OAB A -- outbound binding --> relay <-- outbound binding -- OAB B
```

The relay is a deployment option, not a protocol dependency. Cloudflare Worker
and Durable Object are possible implementations, but the ADR does not make
Cloudflare APIs, pricing, or free-tier behavior part of the OAB contract.

### 2.2 OAB Delegation Profile normative requirements

The profile is mandatory for an OAB peer; it is not optional metadata. It must
be represented by explicitly declared, versioned, negotiable extensions that a
peer can reject. The profile must define at least:

- **Authority attenuation:**
  `caller_grants intersection policy_A intersection policy_B intersection
  request_capabilities intersection deployment_limits`; an empty intersection
  is denied.
- **Isolation:** no credential, session, environment, arbitrary host path, or
  live tool-handle forwarding.
- **Depth and scope:** maximum delegation depth one, bounded text input,
  deadline, runtime/output budget, requested capabilities, and read-only default.
- **Idempotency:** normalized request identity, nonce replay protection,
  duplicate task reuse, and rejection of conflicting reuse.
- **Durable observation:** event sequence/cursor, retention, reconnect replay,
  gap detection, terminal-event durability, and no duplicate side effect from
  observation or retry.
- **Outcome semantics:** explicit `unknown` and `expired` outcomes; neither may
  be silently converted into success or blindly retried.
- **Provenance:** signed request/result digest binding, executor identity,
  effective capabilities, artifact ownership, side effects, and audit record.
- **Conformance:** a peer that does not negotiate the required profile must be
  rejected fail-closed; A2A core authentication or an Agent Card signature is
  not a substitute for these requirements.

The profile must not redefine A2A core task lifecycle, operation names, or
artifact semantics. If any required guarantee can only be implemented by
rewriting those core semantics, the A2A gate fails and the implementation must
use the custom OAB fallback instead.

### 2.3 Implementation gate and fallback

Before selecting a normative binding, run a time-bounded protocol spike. The
spike is an implementation gate, not an open-ended research phase, and must
produce a short conformance report with pass/fail evidence for all three gates:

1. **Replay gate:** run the same `create -> progress -> cancel -> disconnect ->
   reconnect` cases over HTTP/SSE and WSS where applicable. Durable cursors must
   replay every event, including the terminal event, without restarting the ACP
   task or repeating a side effect.
2. **Relay gate:** exercise direct and dual-outbound relay topologies. Measure
   connection establishment, reconnect, backpressure, cancellation races,
   latency, load-balancer/firewall compatibility, monitoring, and key rotation.
   The selected A2A binding must not be materially more complex or less
   operable than the WSS baseline without an explicit accepted trade-off.
3. **Profile gate:** count required fields, endpoints, and extensions and verify
   that the profile remains a thin, explicitly negotiable addition. It must not
   replace A2A lifecycle semantics or become a second protocol.

A gate failure is conclusive: the implementation falls back to the custom OAB
protocol and records the failed criterion. There is no second indefinite debate
round. A gate pass permits adoption of A2A 1.0 plus the OAB profile, with the
selected binding and its conformance tests recorded before implementation is
called interoperable.

### 2.4 Task identity and result mapping

The caller supplies an idempotent `request_id`; the callee creates an immutable
task identity:

```text
request_id -> A2A taskId -> turn_id -> event sequence/cursor
```

- `request_id` deduplicates task creation at OAB B.
- A2A `taskId` is the immutable storage and reconnect key generated by OAB B.
- `task_path` is an optional human-readable route such as `/root/security`; it
  can change without changing task ownership or event history.
- `turn_id` identifies an execution turn if the task later supports follow-up
  work.
- `(taskId, turn_id, sequence)` identifies an event; the OAB profile's durable
  `cursor` allows replay after reconnect.

The caller waits by reading A2A events or task state. Reconnecting with the same
`taskId` resumes observation; it does not submit a second task. A terminal
result includes at least `status`, `summary`, `error`, `artifacts`, and
`executed_by`.

## 3. MVP scope

The MVP validates the remote delegation user story without attempting to build a
full distributed workflow engine.

### 3.1 In scope

- Explicitly configured, deny-by-default OAB peers.
- Authenticated peer connections using mTLS or signed short-lived agent tokens.
- One parent OAB submitting a bounded task to one remote worker OAB.
- Text task input, a deadline, output/runtime budget, and requested capabilities.
- Read-only repository work by default.
- A local ACP session on OAB B for actual execution.
- `create`, `get`, event wait/replay, and cancellation lifecycle operations.
- Durable request/task identity, idempotency, reconnect, and bounded event
  retention.
- Streamed progress followed by one structured terminal result.
- Callee-owned artifact and side-effect provenance in the result envelope.
- A maximum delegation depth of one: the remote worker cannot delegate again.
- Per-peer and global concurrency limits with backpressure.
- Negative tests for unauthorized peers, capability widening, duplicate tasks,
  timeout, cancellation, reconnect, worker failure, and credential leakage.

A minimal request is conceptually:

```json
{
  "protocol_version": 1,
  "request_id": "parent-request-123",
  "target_peer": "oab-b",
  "task": "Review this diff for security issues",
  "capabilities": ["repo:read"],
  "deadline_ms": 300000,
  "budget": {"max_output_bytes": 200000},
  "delegation_depth": 0,
  "nonce": "unique-request-nonce",
  "idempotency_key": "parent-request-123"
}
```

The schema deliberately has no credential, session, raw environment, arbitrary
host path, or live tool-handle field.

### 3.2 Explicit non-goals

- In-process-only subagents: the user story is remote inter-OAB execution.
- Delegation through Discord, Slack, webhooks, or other chat channels.
- Recursive delegation, arbitrary DAG workflow, or unbounded fan-out.
- Dynamic peer discovery, automatic trust enrollment, or cross-organization
  federation.
- Shared conversation history or implicit access to the parent's session.
- Shared writable worktrees or parallel writers. Read-heavy exploration and
  testing are the initial use cases.
- Arbitrary remote tool grants, credential forwarding, or capability expansion.
- Durable workflow scheduling that guarantees recovery after every worker crash.
- A Cloudflare-specific relay implementation in the core OAB repository.

## 4. Lessons from Codex multi-agent v2

Codex multi-agent v2 is useful as an orchestration design reference, not as the
OAB wire contract. OAB should adopt the semantics that solve the user story and
avoid copying an experimental tool schema.

| Codex v2 lesson | OAB decision |
| --- | --- |
| Delegation is a task tree, not just `delegate(prompt) -> text`. | Model a remote task explicitly. Use immutable `task_id` for storage and a readable `task_path` for routing and UI. MVP depth is one. |
| Canonical task paths make routing and ancestry visible. | Accept an optional path for human-facing routing, but never use it as the permanent identity; events and reconnects use `task_id`. |
| Spawn, message, follow-up, interrupt, wait, and discovery are distinct operations. | Keep create, state/events, and cancel distinct in the MVP. If follow-up turns are added, `message`, `followup`, and `interrupt` remain separate operations rather than one overloaded flag. |
| Waiting is an event/mailbox wake-up, not a giant tool response containing all history. | Return a task identity immediately and stream/replay events with sequence numbers and cursors. The caller can fetch the terminal result separately. |
| A worker owns its own context/thread. | Default context policy to `none`; do not copy Discord/private connector context or the parent's session. Future policies must be explicit (`none`, `summary`, or selected references). |
| Concurrency is bounded globally. | Add both per-root and process-wide active-delegation limits, plus deadline, output, and budget limits. Do not consume the ordinary chat session pool without a separate delegation budget. |
| Child agents inherit a restricted runtime, not arbitrary new authority. | Effective authority is the intersection of caller grants, OAB A delegation policy, OAB B policy, request capabilities, and deployment limits. The child cannot widen it. |
| Progress and completion are lifecycle events. | Preserve explicit `accepted`, `running`, `completed`, `failed`, `cancelled`, `expired`, and `unknown` states and make reconnect/replay first-class. |
| Encrypted inter-agent messages can reduce auditability. | Encrypt the transport when needed, but persist a redacted human-readable audit record containing caller, callee, task, capabilities, decision, and outcome. |
| Experimental implementation details should not become a public contract. | Define an OAB-owned versioned delegation protocol and a backend-neutral coordinator; map Codex-like semantics to it rather than exposing Codex names or IDs. |

The main lesson is that delegation is a controlled task-tree runtime with
mailbox-style lifecycle events, not a prompt-forwarding helper. The main OAB
addition is the remote trust, reconnect, provenance, and failure boundary that
a local multi-agent implementation does not provide.

## 5. Context, authority, and provenance

Delegation must reduce authority at every boundary:

```text
effective_authority =
    caller_grants
  intersection OAB A delegation policy
  intersection OAB B callee policy
  intersection request capabilities
  intersection deployment limits
```

An empty intersection is denied. OAB B executes only with its own locally
provisioned identity. No credentials, session handles, environment maps, live
tool objects, or arbitrary host paths cross the connection.

The result envelope is signed by OAB B and binds the request to its outcome:

```json
{
  "protocol_version": 1,
  "request_id": "parent-request-123",
  "task_id": "callee-task-456",
  "status": "completed",
  "summary": "No security issues found",
  "executed_by": "oab-b-agent",
  "effective_capabilities": ["repo:read"],
  "request_digest": "sha256:...",
  "result_digest": "sha256:...",
  "artifacts": [],
  "side_effects": [],
  "attestation": {"algorithm": "ed25519", "signature": "..."}
}
```

The caller may summarize the result, but may not rewrite `executed_by`,
effective capabilities, side effects, or artifact ownership. A callee-created
artifact remains callee-owned; a caller-created derivative receives a new owner
and a provenance link.

The MVP rejects, fail-closed:

- unknown peers or invalid authentication;
- `delegation_depth > 0`;
- unknown capabilities or capability widening;
- credential/session/environment/tool-handle fields;
- reused nonce or conflicting idempotency key;
- expired or malformed requests;
- invalid result signature, digest, executor identity, or artifact ownership.

Nonce/idempotency reservation and the audit event must be committed before
execution. If reservation or audit persistence fails, OAB B must not create a
worker session, call a tool, or create an artifact.

## 6. Lifecycle and failure semantics

The lifecycle is:

```text
received -> validated -> accepted -> running -> terminal
                                  |             |
                                  |             +-> completed
                                  |             +-> failed
                                  |             +-> cancelled
                                  |             +-> expired
                                  |             +-> unknown
                                  +-> rejected
```

`unknown` is required when OAB B cannot prove whether an external side effect
occurred. OAB A must not silently convert `unknown`, `failed`, `cancelled`, or
`expired` into success or blindly retry the task.

Cancellation is best effort: OAB B records the request, asks its local ACP
session to stop, and emits `cancelled` only after the worker stops. A transport
disconnect does not mean success, cancellation, or permission to create a
second task. OAB A reconnects with `task_id` and its last cursor.

Every terminal task persists its outcome digest and signed envelope. A duplicate
request with the same normalized request returns the existing task or result;
a conflicting request with the same idempotency key is rejected.

## 7. Alternatives considered

### Forward a prompt through a chat platform

Rejected for the formal path. Chat messages do not provide reliable task
identity, cancellation, replay, peer authentication, or provenance. Discord can
still be used to notify a human about a delegated task.

### Expose ACP directly to the remote agent

Rejected. ACP is OAB B's local runtime boundary, not a stable inter-OAB
contract. Direct exposure would leak local process and credential assumptions.

### Use MCP for the outer protocol

Rejected for this user story. MCP models agent-to-tool/resource access; this
feature models OAB agent-to-remote-OAB task ownership, progress, cancellation,
and result provenance. OAB B may use MCP internally, but it is not the
inter-OAB delegation protocol.

### A2A 1.0 plus OAB Delegation Profile (selected direction; gate pending)

The selected direction is A2A 1.0 as the inter-OAB wire candidate, subject to
the implementation gate in Section 2.3. A2A is explicitly designed for
communication between independent, potentially opaque agents without exposing
their internal state, memory, or tools. Its task, message, artifact, Agent Card,
authentication, cancellation, and asynchronous update model overlaps
substantially with this ADR.

The conceptual mapping is direct:

| OAB delegation operation | A2A operation or object |
| --- | --- |
| Describe a remote peer | `AgentCard` and, when enabled, `GetExtendedAgentCard` |
| Create a remote task | `SendMessage`, normally with `returnImmediately: true` |
| Read task state/result | `GetTask` and the A2A `Task` object |
| Stream progress and artifacts | `SendStreamingMessage` or `SubscribeToTask` |
| Cancel a task | `CancelTask` |
| Follow-up input | `Message` associated with `taskId` and `contextId` |
| Structured outputs | `Artifact` and `Part` |

A2A also supports direct configuration of an Agent Card, which is compatible
with the MVP's configured, deny-by-default peer registry. An OAB deployment
can publish cards privately or use a preconfigured card URL; dynamic discovery
and federation remain out of scope.

A2A is not a drop-in replacement for the complete OAB contract. The v1.0
standard bindings are HTTP+JSON, JSON-RPC over HTTP, and gRPC, with SSE for
streaming; WebSocket is a custom binding. A custom binding must preserve all
A2A core operations, data-model semantics, event ordering, reconnection
behavior, authentication integration, and Agent Card declaration. Therefore,
the MVP should not select WebSocket merely because it is convenient if an
A2A standard binding meets the topology requirement. If a persistent WSS
transport remains necessary, it should be specified as an A2A custom binding,
not as an unrelated OAB protocol.

A2A also does not replace OAB-specific policy. Its authentication and
authorization model relies on deployment and web-security practices, and its
Agent Card signature authenticates the card rather than signing an individual
task result. The core protocol does not define OAB's authority intersection,
credential/session/environment isolation, depth-one rule, signed request/result
binding, artifact ownership, or durable event cursor. A2A streaming supports
ordered events and resubscription, but disconnected clients may miss messages;
OAB's durable cursor and replay guarantee must remain an OAB requirement.
A2A's extension and metadata mechanisms are appropriate places to declare
these requirements, but credentials and live handles must never be forwarded
through them.

This is the normative direction for the adapter, not a claim that raw A2A
already satisfies OAB's contract. The selected binding becomes normative only
when the Section 2.3 gate and conformance tests pass. Until then, the transport
and method boundary remains implementation-neutral; if the gate fails, OAB
falls back to the custom protocol and must not claim standard A2A
interoperability.

### Implement only local subagents

Rejected as the MVP boundary. A local coordinator may be useful later, but it
does not let any running OAB agent connect to another remote OAB as requested.

### Stateless HTTP request/response

Rejected as the only interaction model. A2A HTTP/JSON and JSON-RPC bindings
provide task creation, state retrieval, and cancellation over HTTP, while SSE
provides progress and artifact updates. The gate must verify whether that
standard binding satisfies OAB's durable replay and outbound-only relay needs.
A persistent WSS transport remains an optional A2A custom binding or the
fallback custom OAB protocol if the gate fails.

## 8. Prior art

### Codex multi-agent v2

Codex v2 provides the most relevant orchestration lessons: canonical task
paths, separate lifecycle operations, mailbox/event wake-ups, explicit context
inheritance, bounded concurrency, and structured collaboration events.

- [Codex subagents](https://developers.openai.com/codex/concepts/subagents)
- [Codex multi-agent v2 handlers](https://github.com/openai/codex/tree/main/codex-rs/core/src/tools/handlers/multi_agents_v2)
- [Codex multi-agent specifications](https://github.com/openai/codex/blob/main/codex-rs/core/src/tools/handlers/multi_agents_spec.rs)
- [Codex auditability issue](https://github.com/openai/codex/issues/28058)

### OpenClaw

OpenClaw demonstrates explicit per-agent routing, separate state/workspace
boundaries, and opt-in agent-to-agent messaging. OAB adopts those boundaries
but adds a remote protocol, request/result binding, and callee-signed
provenance.

- [OpenClaw multi-agent routing](https://github.com/openclaw/openclaw/blob/main/docs/concepts/multi-agent.md)
- [OpenClaw repository](https://github.com/openclaw/openclaw)

### Hermes Agent

Hermes demonstrates fresh child conversations, bounded concurrency,
cancellation/background completion, and explicit `unknown` handling when a
worker may have had side effects. OAB adopts these lifecycle principles while
keeping the inter-OAB trust and capability boundary explicit.

- [Hermes subagent delegation](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/features/delegation.md)
- [Hermes Agent repository](https://github.com/NousResearch/hermes-agent)

## 9. Acceptance criteria for the implementation

The implementation that follows this ADR is acceptable only when it demonstrates
all of the following:

1. Agent A can submit a bounded task to a configured remote OAB B, wait for
   progress, and receive one structured terminal result.
2. OAB B authenticates and authorizes the peer before creating a worker session.
3. A duplicate request reconnects to the existing task instead of executing it
   twice; a conflicting request is rejected.
4. Progress and terminal events can be replayed from a durable cursor after a
   reconnect on the selected binding; the implementation does not restart an
   ACP task or repeat a side effect during observation recovery.
5. Cancellation, deadline expiry, worker failure, and `unknown` outcomes are
   distinguishable and tested.
6. The child receives only its locally allowed environment and capabilities;
   parent credentials, sessions, and live handles are absent.
7. No recursive delegation, capability widening, credential forwarding, or
   caller-owned callee artifact claim is possible through the MVP API.
8. The signed result binds `request_digest`, `result_digest`, effective
   capabilities, executor identity, task identity, and artifact/side-effect
   provenance.
9. Per-peer and global concurrency/backpressure limits prevent delegation from
   starving ordinary OAB chat work.
10. A human-readable audit record remains available even when the transport is
    encrypted.
11. The replay gate passes the same create/progress/cancel/disconnect/reconnect
    cases for each candidate binding, including complete terminal-event replay,
    no ACP restart, and no duplicate side effect.
12. The relay gate records direct and dual-outbound results for connection setup,
    reconnect, backpressure, cancellation races, latency, load-balancer and
    firewall compatibility, monitoring, and key rotation; no material
    operability regression is accepted without an explicit trade-off.
13. The profile gate records required fields, endpoints, and extensions and
    proves that the profile is thin, explicitly negotiable, rejectable, and does
    not redefine A2A lifecycle or artifact semantics.
14. A failed gate records the failed criterion and selects the custom OAB
    fallback; the implementation must not claim standard A2A interoperability.

## 10. Consequences

### Positive

- Any running OAB agent can use another running OAB agent as a bounded remote
  specialist without pretending it is a chat participant.
- The user-facing operation is simple while the host retains policy,
  authentication, retry, and audit responsibilities.
- Codex v2's useful task-tree and mailbox semantics are available without
  coupling OAB to an experimental vendor wire format.
- Read-only parallel exploration, review, testing, and summarization become
  possible before introducing write coordination.

### Costs and residual risks

- Remote identity, key rotation, durable cursors, and audit storage add
  operational complexity.
- A worker crash during an external side effect can leave the result `unknown`;
  callers must reconcile rather than blindly retry.
- The MVP requires preconfigured peers and does not solve global discovery or
  federation.
- Shared writable worktrees and multi-writer coordination remain future design
  work; the safe default is read-only delegation.

## 11. Implementation sequence

1. Define the A2A data-model mapping, version negotiation, OAB Profile
   extension identifiers, stable errors, task identity, and context policy.
2. Implement the backend-neutral delegation coordinator and local task registry.
3. Run the time-bounded spike against A2A HTTP/JSON or JSON-RPC + SSE and the
   WSS baseline/custom binding where needed; publish replay, relay, and profile
   gate evidence.
4. If the gate passes, select and declare the A2A binding in the Agent Card. If
   it fails, record the criterion and select the custom OAB fallback without
   claiming A2A interoperability.
5. Bridge OAB B tasks to the existing ACP connection/session pool without
   changing the child environment allowlist.
6. Add idempotency, durable cursors, signed result validation, audit records,
   and the OAB Profile conformance tests.
7. Add negative and end-to-end tests covering the acceptance criteria, then
   evaluate relay deployment and richer follow-up/message operations only after
   the depth-one read-only MVP is stable.

## 12. References

- [A2A v1.0 specification](https://a2a-protocol.org/latest/specification/)
- [A2A streaming and asynchronous operations](https://a2a-protocol.org/latest/topics/streaming-and-async/)
- [A2A protocol definition and schema](https://a2a-protocol.org/latest/definitions/)
- [A2A protocol repository](https://github.com/a2aproject/A2A)
- [OpenAB ACP connection and session pool](../../crates/openab-core/src/acp/connection.rs)
- [OpenAB child-process environment policy](../../AGENTS.md#3-security--child-process-environment)
- [Agent Client Protocol](https://agentclientprotocol.com/)
- [RFC 8785 - JSON Canonicalization Scheme](https://www.rfc-editor.org/rfc/rfc8785)
- [Ed25519 signatures](https://www.rfc-editor.org/rfc/rfc8032)

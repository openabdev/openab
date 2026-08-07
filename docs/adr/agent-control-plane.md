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
   with backoff. The CP itself is stateless enough that a restart only means
   re-registration.

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

```json
{
  "method": "cp/delegate",
  "params": {
    "delegation_id": "d-01J...",
    "target": { "name": "worker-1" },
    "prompt": "…",
    "chain": ["koudu"],
    "deadline": "2026-08-06T22:45:00Z"
  }
}
```

- `target` — exact `name` or a `labels` selector (CP schedules among matches)
- `chain` — the full delegation ancestry, appended at every hop. Enables
  cycle rejection (target already in chain), depth enforcement, fan-out
  budgets, and audit tracing back to the human-facing root.
- `deadline` — propagated absolute deadline. A child's timeout can never
  exceed its parent's remaining budget, so orphaned workers cannot keep
  consuming tokens after the root gave up.

### Result delivery is protocol-mandatory

Adopting Kiro's `summary` lesson at the protocol level: a delegation is not
complete until the serving runtime returns a structured result frame
(`cp/delegate_result` with status, result text, and error detail on failure).
The serving **runtime** emits this frame when the agent's turn ends — result
delivery never depends on the sub-agent model "remembering" to report.

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
| `spawn_agent` | Delegate a task. Blocking (waits up to deadline) or async (returns `delegation_id` immediately). |
| `check_delegation` | Status / result by `delegation_id`. |
| `list_agents` | Registry view for the caller's namespace (names, types, labels, availability) — lets the model discover targets by label. |
| `cancel_delegation` | Cancel an in-flight delegation. |

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
  `openab agent status <id>` when a delegation hangs
- **Hooks & cron:** lifecycle hooks and cron jobs can fire
  `openab agent spawn …` without new plumbing
- **Escape hatch** for backends where MCP injection proves awkward

### Explicitly deferred from v1

Kiro-style session-management primitives — inbox messaging, `interrupt`,
`inject_context`, group broadcast — arrive later behind the same socket and
facade without changing anything shipped in v1.

---

## 7. Security

- **No CP credentials in the agent process.** `OPENAB_CP_KEY` lives in the
  OAB runtime env; agent subprocesses keep the existing `env_clear`
  whitelist. The UDS path is the only thing the child needs; filesystem
  permissions on the socket are the local auth boundary. The local API is
  never exposed on TCP.
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

1. **Streaming intermediate output** — should `cp/delegate` stream
   `session/update`-style chunks back to the primary, or only the final
   result frame? v1 leans final-only; streaming is additive.
2. **CP high availability** — single instance + fast re-registration is
   acceptable for v1; is active/standby needed before multi-tenant use?
3. **Human-visibility directives** — should a primary be able to mirror
   selected delegation traffic into a Discord thread (observability) via
   existing output directives?
4. **AgentCore/remote runtimes** — an `agentcore-acp`-backed OAB registers
   like any other runtime; verify deadline propagation across the SDK
   boundary.

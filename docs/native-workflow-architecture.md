# OpenAB-Native Three-Agent Coding Workflow

Canonical architecture for the OpenAB-native three-agent coding workflow
(`workflow 20260818-openab-automatic-three-agent-handoff`). This document
pins the production-authoritative path; consumer code and external
documentation MUST conform.

## 1. Authority

OpenAB is the **native coding workflow authority** for this path. The
OpenAB Rust daemon owns the canonical assignment state, the deterministic
transition id, the validator, and the targeted Discord delivery. Earlier
AAP/OpenClaw Python paths are **explicitly out of scope** for the
production native workflow.

## 2. Native execution path

The flow runs inside a single per-agent OpenAB daemon:

```text
Discord
   │
   ▼
per-agent OpenAB daemon
   │
   ▼
ACP
   │
   ▼
WorkflowTurnHookInputs
   │
   ▼
openab_core::workflow::WorkflowService
   │
   ▼
daemon-local ChatAdapterWorkflowMessenger
   │
   ▼
Discord
```

Steps, with concrete types where helpful:

1. **Discord inbound turn** arrives at the agent's daemon via the
   configured `DiscordAdapter` (per-agent bot token + role id list).
2. **ACP turn** funnels the turn through the OpenAB dispatcher.
3. **`WorkflowTurnHookInputs`** carries the terminal-turn text buffer,
   the pinned `project_id` / `project_root`, and the resolved agent
   identity (`AgentIdentity`).
4. **`openab_core::workflow::WorkflowService::on_turn_complete`** parses
   the untrusted `<role_completion>` block, loads the trusted
   `WorkflowAssignment`, inspects the `TransitionLedger`, runs the
   10-check `validator`, and — on `Accepted` — runs the 12-step commit
   protocol.
5. **`ChatAdapterWorkflowMessenger`** (`workflow::handoff`) is the
   daemon-local Discord messenger. It renders the
   `<workflow_activation>` body, pins `allowed_mentions = [recipient]`
   for that thread, and emits the targeted REST call on the daemon's
   own adapter.
6. **Discord** receives the message, surfaces the single recipient
   ping, and wakes the next daemon's `MultibotMentions` check.

No cross-daemon routing is involved. No Python process participates.
No `<@USER_ID>` token is invented outside the canonical identity map.

## 3. Per-daemon ownership

Each OpenAB daemon owns its own:

| Resource | Source of truth |
| --- | --- |
| Discord bot token | `config.toml` `[[connectors]]` entry |
| `DiscordAdapter` | constructed in `openab` main from `config.toml` |
| `ChatAdapterWorkflowMessenger` | constructed in `openab` main wrapping the adapter |
| `WorkflowService` | constructed in `openab` main wrapping the messenger |

Because every daemon already owns its own Discord bot token and
`DiscordAdapter`, native handoff needs no cross-daemon sender selection
and no shared socket. The next-stage recipient is resolved entirely
from the canonical `AgentIdentity` map inside the daemon.

## 4. Canonical persistence

OpenAB writes two artifacts under the project root:

```text
<project_root>/.openab/workflow_assignment.json   # WorkflowAssignment (v2 schema)
<project_root>/.openab/workflow_transitions.json  # TransitionLedger (max 256 rows)
```

These paths are owned by `openab_core::workflow::assignment` and
`openab_core::workflow::ledger`. Python paths such as
`.agents/workflow_assignment.json` and AAP-side SQLite ledgers are NOT
the authority for the native workflow.

## 5. State flow

The canonical OpenAB state machine:

```text
PRIMARY_ACTIVE
   │
   │ exactly one handoff (Accepted + Discord send)
   ▼
VERIFIER_ACTIVE
   │
   │ exactly one handoff
   ▼
FINAL_REVIEWER_ACTIVE
   │
   │ no coding-agent handoff
   ▼
TECH_LEAD_WAIT
```

A single bounded correction cycle is allowed when the verifier
rejects:

```text
PRIMARY_ACTIVE → VERIFIER_ACTIVE → PRIMARY_ACTIVE → VERIFIER_ACTIVE → FINAL_REVIEWER_ACTIVE → TECH_LEAD_WAIT
```

Roles and results:

- Roles: `PRIMARY`, `VERIFIER`, `FINAL_REVIEWER`.
- Results from `PRIMARY`: only `COMPLETE`.
- Results from `VERIFIER`: `PASS` (advance) or `FAIL` (bounded correction).
- Results from `FINAL_REVIEWER`: only `PASS`.

## 6. Completion contract

The bridge contract between the LLM-authored response and the trusted
validator is a single XML-like block in the agent's terminal turn:

```text
<role_completion>
role: <PRIMARY|VERIFIER|FINAL_REVIEWER>
result: <COMPLETE|PASS|FAIL>
workflow_id: <stable identifier>
project_id: <stable identifier>
project_root: <absolute path>
</role_completion>
```

The validator (Phase 2, 10 checks) compares the block against the
trusted assignment and rejects with a typed `RejectReason` on any
mismatch. Plain-text `VERIFIER_PASS`, bare `HANDOFF`, or
plain `@ArthurGemini` does NOT count.

## 7. Terminal FINAL_REVIEWER semantics

A terminal `FINAL_REVIEWER_ACTIVE → TECH_LEAD_WAIT` PASS:

- commits the transition to `TECH_LEAD_WAIT`;
- increments `workflow_revision` by one;
- creates a `Delivered` ledger row (subject to the dispatcher
  outcome type);
- sends **NO** downstream bot message;
- the canonical `target_user_id` is `None`;
- the canonical `message_id` is `None`.

The Tech Lead is not a bot. `MultibotMentions` does not fire.
`DiscordHandoffDeliveryService` is not invoked. `WorkflowService`
records the commit in the ledger and exits.

## 8. Out of scope

- The AAP / OpenClaw Python auto-handoff path is **not** the authority
  for the native workflow. It may be retained as an optional,
  caller-driven bridge for non-OpenAB agents, but it MUST NOT drive
  the native daemon or mutate `<project_root>/.openab/*`.
- Legacy `.agents/workflow_assignment.json` is the AAP assignment
  format and is explicit out of scope for the native path. OpenAB
  reads `.openab/workflow_assignment.json` only.
- The Python socket bridge (`<daemon_data>/openclaw/handoff-notifications.db`)
  is not part of the native path.

## 9. Phase 4.1 live acceptance

Phase 4.1 — native OpenAB three-agent auto-handoff — passed full live
end-to-end acceptance on 2026-08-18. Authoritative transitions:

```text
45b455bf583e0b668313219ebfeeb424   PRIMARY_ACTIVE     PRIMARY        COMPLETE   → VERIFIER_ACTIVE     → ArthurCodex
bc630654407147af5923fd8938b77476   VERIFIER_ACTIVE    VERIFIER       PASS       → FINAL_REVIEWER_ACTIVE → ArthurGemini
fe157ef40968b7b2630e8fdc3802e435   FINAL_REVIEWER_ACTIVE FINAL_REVIEWER PASS    → TECH_LEAD_WAIT      → no downstream message
```

Final state: `TECH_LEAD_WAIT`, `workflow_revision=3`,
`defect_loop_count=0`. See
[`docs/milestones/20260818-openab-automatic-three-agent-handoff/README.md`](milestones/20260818-openab-automatic-three-agent-handoff/README.md)
for the closure record.

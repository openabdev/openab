# Phase 4.1 — Native OpenAB Three-Agent Auto-Handoff

Closure record for the native OpenAB three-agent auto-handoff
(`workflow 20260818-openab-automatic-three-agent-handoff`). Phase 4.1
passed full live end-to-end acceptance on **2026-08-18**.

See [`../native-workflow-architecture.md`](../native-workflow-architecture.md)
for the canonical architecture.

## 1. Live evidence

### 1.1 Discord thread

```text
thread_id: 1539431300317061231
```

### 1.2 Canonical transitions (live)

| transition_id (prefix) | state | role | result | next_state | recipient |
| --- | --- | --- | --- | --- | --- |
| `45b455bf583e0b668313219ebfeeb424` | `PRIMARY_ACTIVE` | `PRIMARY` | `COMPLETE` | `VERIFIER_ACTIVE` | `ArthurCodex` |
| `bc630654407147af5923fd8938b77476` | `VERIFIER_ACTIVE` | `VERIFIER` | `PASS` | `FINAL_REVIEWER_ACTIVE` | `ArthurGemini` |
| `fe157ef40968b7b2630e8fdc3802e435` | `FINAL_REVIEWER_ACTIVE` | `FINAL_REVIEWER` | `PASS` | `TECH_LEAD_WAIT` | *(no downstream message)* |

`WorkflowService` log on terminal transition:

```text
next_stage=TECH_LEAD_WAIT
target_logical_name=None
target_user_id=None
message_id=None
```

### 1.3 Final state

```text
state=TECH_LEAD_WAIT
workflow_revision=3
defect_loop_count=0
last_transition_id=fe157ef40968b7b2630e8fdc3802e435
last_delivery_message_id=null
```

## 2. Acceptance gates

| Gate | Result | Evidence |
| --- | --- | --- |
| A1 — Claude → Codex automatic activation | **PASS** | `45b455bf...` produced exactly one Discord send (`message_id != null`), Codex daemon observed the `<workflow_activation>` and routed the ACP turn |
| A2 — Codex → Gemini automatic activation | **PASS** | `bc630654...` produced exactly one Discord send, Gemini daemon woke via `MultibotMentions` |
| A3 — Gemini terminal transition | **PASS** | `fe157ef...` committed `TECH_LEAD_WAIT`; `target_user_id=None`, `message_id=None` |
| A12 — Phase 1/2 surface (`WorkflowService` + validator + ledger) | **PASS** | `crates/openab-core/src/workflow/service.rs` Phase 4 protocol executed end-to-end |
| A13 — Workflow-role gate | **PASS** | `crates/openab-core/src/workflow/context.rs::phase3_a13_decide` respected assignment state; no unauthorised bot activation |
| Project pin / `SessionPool` routing | **PASS** | `<project_root>/.openab/workflow_assignment.json` resolved by `openab_core::workflow::assignment::load_assignment`; mismatched project roots rejected |
| Daemon-local messenger ownership | **PASS** | Each daemon constructed its own `ChatAdapterWorkflowMessenger` from its own `DiscordAdapter`; no shared socket / cross-daemon sender selection observed |
| Terminal no-delivery semantics | **PASS** | Terminal `FINAL_REVIEWER → TECH_LEAD_WAIT` produced zero `DiscordHandoffDeliveryService` calls and zero `<@USER_ID>` tokens |

## 3. What did NOT count as evidence

- Gemini's prior bounded review of the AAP / Python auto-handoff
  implementation (Phase 4 MCP / OpenClaw integration work) is **NOT**
  evidence of the native Rust implementation passing. The native
  workflow authority is `openab_core::workflow::WorkflowService`,
  not the AAP / OpenClaw Python auto-handoff path.
- The Python `runtime.application.auto_handoff` ledger / validator /
  transition-id machinery is the durable persistence contract for the
  AAP path; native OpenAB uses its own
  `openab_core::workflow::TransitionLedger`.

## 4. Architecture boundary

Phase 4.1 confirms the following boundary:

```text
Discord
   │
   ▼
per-agent OpenAB daemon
   │
   ▼
ACP → WorkflowTurnHookInputs → openab_core::workflow::WorkflowService
   │
   ▼
daemon-local ChatAdapterWorkflowMessenger
   │
   ▼
Discord
```

The AAP / OpenClaw Python auto-handoff path is separate optional work
and is **not** the authority for this native path. Future docs may
describe a clean interop boundary but Phase 4.1 does not.

## 5. Persistence artifacts

```text
<project_root>/.openab/workflow_assignment.json   ← final state (TECH_LEAD_WAIT, rev=3)
<project_root>/.openab/workflow_transitions.json  ← ledger with three delivered rows
```

## 6. Follow-up recommendations

Document-level follow-up only; no native behaviour change required:

1. Phase 4.2 — pin the canonical Phase 4.1 acceptance record into the
   project-level README so out-of-tree consumers cannot miss it.
2. Cross-repo docs — when ai-workstation resumes Phase 5 documentation
   sync, mirror the `native-workflow-architecture` summary in the
   ai-workstation canonical docs and explicitly retire any previous
   Python auto-handoff "native" claims.

## 7. Provenance

This record was produced as part of the **Phase 4.1 closure, cleanup,
and documentation sync** workflow. No commit, push, merge, or daemon
restart was performed; documentation changes only.

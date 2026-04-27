# OpenAB + GBrain Reference Architecture

Shared persistent memory for OpenAB multi-agent deployments.

## Background — The Isolation Problem

When running multiple AI agents through OpenAB (e.g. Kiro + Claude Code + Copilot), each agent pod operates in complete isolation:

- **No shared memory** — Agent A discovers a critical finding, but Agent B has no way to access it.
- **No cross-agent context** — Handing off a task means losing all the context the previous agent built up.
- **No persistent knowledge** — Everything an agent learns is gone when the session ends. The next session starts from zero.

In a single-agent setup this is tolerable. In a multi-agent deployment — which is the whole point of OpenAB — it becomes a serious bottleneck. Agents duplicate work, miss context, and cannot collaborate on anything beyond the current conversation.

What we need is a **shared brain**: a persistent, queryable knowledge layer that all agents can read from and write to, in real time.

## GBrain — A Shared Brain for AI Agents

[GBrain](https://github.com/garrytan/gbrain) is an open-source persistent memory system built on PostgreSQL + pgvector. It was designed specifically for AI agent workflows.

What makes it a good fit:

- **MCP native** — Exposes 30+ tools via the standard MCP protocol. Any ACP CLI that supports MCP (Kiro, Claude Code, Copilot, Gemini, OpenCode, etc.) can use it out of the box — no custom integration needed.
- **Hybrid search** — Combines vector similarity, keyword matching, and knowledge graph traversal. Agents get high-quality retrieval, not just embedding cosine distance.
- **Structured knowledge** — Pages with markdown + frontmatter + tags, typed links between entities (`assigned_to`, `depends_on`, …), and chronological timelines per entity.
- **Simple deployment** — One PostgreSQL instance, one CLI binary. No external services, no API keys, no complex infrastructure.

```
Capabilities:
  • Pages        markdown + frontmatter + tags
  • Search       hybrid (vector + keyword + graph)
  • Links        typed edges (assigned_to, depends_on, …)
  • Timeline     chronological events per entity
  • 30+ tools    exposed via MCP (brain_write, brain_query, …)
```

## How GBrain Fits into OpenAB

The integration is straightforward: each OAB agent pod runs `gbrain serve` as a local MCP server, and all instances point to the same PostgreSQL database. One agent writes a page → all others can query it instantly.

```
  Discord / Slack / Telegram / LINE
    │            │            │
    ▼            ▼            ▼
┌────────┐ ┌────────┐ ┌────────┐
│OAB Pod │ │OAB Pod │ │OAB Pod │   ← any ACP CLI (kiro, claude, copilot, …)
│        │ │        │ │        │
│ gbrain │ │ gbrain │ │ gbrain │   ← MCP server per pod (gbrain serve)
│  serve │ │  serve │ │  serve │
└───┬────┘ └───┬────┘ └───┬────┘
    └──────────┼──────────┘
               ▼
     ┌───────────────────┐
     │  gbrain-postgres  │   pgvector/pgvector:pg16 · 5Gi PVC
     └───────────────────┘
```

**Zero coupling** — GBrain is an add-on, not a dependency. OpenAB works fine without it. You opt in when you need cross-agent memory.

- **OpenAB**: https://github.com/openabdev/openab
- **GBrain**: https://github.com/garrytan/gbrain

### Cross-Agent Workflow Example

```
┌─ Agent A writes ──────────────────────────────────────────┐
│                                                           │
│  gbrain put handoff/task-42   ← structured page           │
│  gbrain link handoff/task-42 agent/B --type assigned_to   │
│                                                           │
├─ Agent B queries ─────────────────────────────────────────┤
│                                                           │
│  gbrain query "what tasks are assigned to me?"            │
│  gbrain get handoff/task-42   ← full context              │
│                                                           │
└───────────────────────────────────────────────────────────┘
```

---

## Deployment

Currently, GBrain is set up manually alongside OpenAB. The Helm chart does not include built-in GBrain support yet, but the `extra*` escape hatches make automation possible.

### Manual Setup

For existing OpenAB deployments. No Helm changes needed.

```
┌─────────────────────────────────────────────────────────┐
│ 1. Deploy PostgreSQL                                    │
│                                                         │
│    kubectl create secret ─► gbrain-postgres-secret      │
│    kubectl apply          ─► Deployment + Service + PVC │
│                                                         │
│    Image: pgvector/pgvector:pg16                        │
│    Env:   POSTGRES_DB=gbrain                            │
│           POSTGRES_USER=gbrain                          │
│           POSTGRES_PASSWORD=(from secret)               │
│    PVC:   5Gi RWO                                       │
│    Svc:   ClusterIP :5432                               │
├─────────────────────────────────────────────────────────┤
│ 2. For each OAB agent pod:                              │
│                                                         │
│    kubectl exec <pod> ──┐                               │
│                         ├─► Install gbrain CLI          │
│    curl install.sh      │   → ~/.local/bin/gbrain       │
│                         ├─► Init database               │
│    gbrain init --url    │   → ~/.gbrain/config.json     │
│                         ├─► Write MCP config            │
│                         │                               │
│    ┌──────────┬─────────────────────────────┐           │
│    │ CLI      │ MCP config path             │           │
│    ├──────────┼─────────────────────────────┤           │
│    │ kiro     │ ~/.kiro/settings/mcp.json   │           │
│    │ claude   │ ~/.claude/server.json       │           │
│    │ copilot  │ ~/.copilot/mcp-config.json  │           │
│    └──────────┴─────────────────────────────┘           │
│                                                         │
│    MCP config content:                                  │
│    {"mcpServers":{"gbrain":{                            │
│      "command":"~/.local/bin/gbrain",                   │
│      "args":["serve"]}}}                                │
│                                                         │
│    ⚠ Note: ~ may not expand in all environments.        │
│    Use $HOME or an absolute path if your MCP client     │
│    or init container does not perform tilde expansion.   │
├─────────────────────────────────────────────────────────┤
│ 3. Verify                                               │
│                                                         │
│    kubectl exec <pod> -- gbrain stats                   │
│    kubectl exec <pod> -- gbrain query "test"            │
└─────────────────────────────────────────────────────────┘
```

Data lives on PVC — survives pod restarts when `persistence.enabled: true` (OpenAB default).

### Future: Helm Integration

The OpenAB Helm chart does not have first-class GBrain support today. For advanced users, the chart exposes `extraInitContainers`, `extraVolumes`, and `extraVolumeMounts` per agent — these can be used to automate the manual steps above at the pod level. A dedicated `gbrain.enabled` Helm value may be considered in a future release.

# The Design of OpenAB

OpenAB (Open Agent Broker) is a lightweight, secure, cloud-native harness that bridges messaging platforms and any [ACP](https://github.com/anthropics/agent-protocol)-compatible coding CLI over stdio JSON-RPC. It connects Discord, Slack, Telegram, LINE, Feishu/Lark, Google Chat — and any future platform — to Kiro CLI, Claude Code, Codex, Gemini, OpenCode, Copilot CLI, and any future agent.

This document describes the design philosophy behind OpenAB — the decisions we made, the decisions we deliberately did not make, and why.

## Core Philosophy

OpenAB's design rests on four pillars.

### 1. Thin Bridge

OpenAB is a **transportation layer**. It moves messages from a platform to an agent and sends responses back. That's it.

```
Platform ──► OpenAB ──► Agent CLI (via ACP stdio)
Platform ◄── OpenAB ◄── Agent CLI
```

OpenAB does not manage agent memory. It does not orchestrate multi-agent workflows. It does not inject system prompts, modify context windows, or govern agent behavior. These are deliberate non-decisions — not missing features.

The orchestration layer and governance layer sit above OpenAB, and they belong entirely to the user. Users decide how their agents collaborate, what memory systems to use, and what rules to enforce. OpenAB stays out of the way.

This is fundamentally different from projects like OpenClaw or Hermes Agent, which bundle memory systems, session context management, and opinionated agent orchestration into the platform itself. OpenAB takes the opposite stance: **the thinner the bridge, the more freedom above it.**

### 2. Multi-Bot Ready

OpenAB is designed from day one for multiple agents to coexist in the same channel.

Each agent runs as an independent pod with its own bot token, its own configuration, and its own session pool. When multiple agents share a channel, OpenAB provides the primitives — `allow_bot_messages`, `trusted_bot_ids`, `@mention` gating, bot turn caps — but never dictates how agents should interact.

Users decide the orchestration strategy:
- Sequential handoff (review bot → deploy bot)
- Parallel collaboration (multiple agents answering the same question)
- Human-in-the-loop (agents propose, human decides)
- Agent-to-agent discussion (A2A for research and convergence)

OpenAB enables all of these patterns without being opinionated about any of them.

### 3. AI-Native

OpenAB expects all operations to be performed through AI.

Installation, upgrades, configuration, troubleshooting — the best practice is to tell your AI agent what you want and let it read the `docs/` directory to figure out how. The documentation under `docs/` is written primarily for AI consumption: each agent has its own standalone guide (`kiro.md`, `claude-code.md`, `codex.md`, etc.), and the config reference is structured for machine parsing.

The recommended workflow:

```
User: "Set up Claude Code as a second agent"
  → Agent reads docs/claude-code.md and docs/multi-agent.md
  → Agent generates the Helm command
  → User reviews and applies
```

This is not a documentation shortcut — it's a design choice. AI agents are better at synthesizing multi-file instructions, resolving environment-specific variables, and adapting commands to the user's context than static step-by-step guides.

### 4. Security by Design

OpenAB was designed to run inside Kubernetes pods from day one. Outside of local development, OpenAB **must** run in a sandboxed environment — this is not a recommendation, it is a design requirement.

Each agent runs in its own pod with:
- **Process isolation** — agents cannot access each other's state
- **Read-only root filesystem** — agents cannot modify the system
- **No host access** — agents cannot reach the host machine's files or network
- **Credential separation** — each agent gets only its own bot token and API keys via Kubernetes Secrets
- **`env_clear()` by default** — agent subprocesses start with a clean environment; only `HOME`, `PATH`, and `USER` are inherited unless explicitly configured

Running OpenAB directly on a host (Linux, macOS, Windows) is supported **only for local development and testing**. For all other use cases, OpenAB must run in a sandbox. The security model assumes pod-level isolation.

This is a fundamental architectural difference. OpenClaw, for example, was originally designed to give agents full access to host resources, and later added sandboxing as an opt-in hardening measure. OpenAB takes the opposite path: the agent is locked inside a sandbox on day one. It cannot break out of the sandbox, and there is no opt-in to escape it. The sandbox is not a default — it is the only option.

## Architecture

```
┌──────────────┐  Gateway WS   ┌──────────────┐  ACP stdio    ┌──────────────┐
│   Discord    │◄─────────────►│              │──────────────►│  coding CLI  │
│              │               │    openab    │◄── JSON-RPC ──│  (acp mode)  │
├──────────────┤  Socket Mode  │    (Rust)    │               └──────────────┘
│   Slack      │◄─────────────►│              │
│              │               └──────┬───────┘
├──────────────┤                      │ WebSocket (outbound)
│   Telegram   │◄──webhook──┐         │
├──────────────┤            ▼         ▼
│   LINE       │◄──webhook──┌──────────────────┐
├──────────────┤            │  Custom Gateway  │
│  Feishu/Lark │◄───WS──────│  (standalone)    │
├──────────────┤            │                  │
│ Google Chat  │◄──webhook──│                  │
└──────────────┘            └──────────────────┘
```

### Two-Tier Platform Adapters

**Native adapters** (Discord, Slack) connect directly from the OpenAB binary. Both use outbound connections only — Discord via WebSocket Gateway, Slack via Socket Mode — so OpenAB never needs an inbound port.

**Gateway adapters** (Telegram, LINE, Feishu/Lark, Google Chat) run through the standalone Custom Gateway service. The gateway handles inbound webhooks and platform credentials; OpenAB connects to it via outbound WebSocket. This separation keeps webhook complexity and platform secrets out of the core binary.

The preferred future direction is for all platforms — including Discord and Slack — to go through the gateway, making OpenAB purely outbound. This is tracked in [ADR: Custom Gateway](adr/custom-gateway.md).

### The ACP Bridge

OpenAB communicates with agent CLIs via the [Agent Client Protocol](https://github.com/anthropics/agent-protocol) — JSON-RPC over stdio. This means:

- **No HTTP server in the agent** — the agent is a subprocess, not a service
- **No network exposure** — communication is over stdin/stdout pipes
- **Any ACP-compatible CLI works** — swap agents by changing one config line

OpenAB handles ACP-level concerns transparently: tool call permission auto-reply, thinking block passthrough, and edit-streaming (live message updates as tokens arrive).

### Session Pool

Each conversation thread gets its own agent subprocess. The session pool manages lifecycle:

- One CLI process per thread
- Configurable max sessions and idle TTL
- Idle eviction when the pool is full
- Per-thread working directories (opt-in via `per_thread_workdir` feature flag)

Sessions are isolated — one thread's agent cannot see another thread's state.

## What OpenAB Is Not

- **Not an agent framework** — it does not provide memory, RAG, tool registries, or prompt management. Use your agent's native capabilities for these.
- **Not an orchestration engine** — it does not decide which agent handles which message, or how agents collaborate. That's the user's design space.
- **Not a platform SDK** — it delegates to existing CLIs rather than reimplementing agent protocols.
- **Not opinionated about AI models** — any ACP-compatible CLI works, regardless of the underlying model provider.

## Why This Matters

The AI agent ecosystem is evolving rapidly. Memory systems, orchestration patterns, and governance frameworks are all in flux. By staying thin, OpenAB avoids coupling to any particular approach and lets users adopt whatever works best for their use case — today and tomorrow.

A user running five agents on OpenAB (Kiro, Claude Code, Codex, Gemini, Copilot) can give each agent completely different steering documents, memory backends, and tool configurations. OpenAB doesn't know or care. It just moves messages.

**OpenAB is a pipe, not a container.** It transports messages without transforming them. The intelligence lives in the agents. The orchestration lives with the user. The bridge just works.

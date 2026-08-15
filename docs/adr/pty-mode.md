# ADR: PTY Mode — Remote Sandboxed Terminal Sessions

- **Status:** Proposed
- **Date:** 2026-08-15
- **Author:** @pahud
- **Related:** [ADR: ACP Server with WebSocket Transport](./acp-server-websocket.md), [ADR: Separate Binaries with Opt-In Unified Build](./unified-binary.md)
- **Implementation:** TBD

---

## 1. Context & Problem

OpenAB today is an ACP broker: every conversation thread maps to an agent subprocess driven over ACP stdio JSON-RPC, managed by the session pool (`crates/openab-core/src/acp/pool.rs`). Users interact through messaging platforms (Discord, Slack, Telegram, …) in a turn-based, message-oriented model.

This model is deliberately thin and safe, but it has a hard observability boundary: **OAB only sees what the agent chooses to emit through ACP**. Structured events (`AgentMessageChunk`, `ToolCall`, permission requests) come through; raw shell output, build logs, scrollback, cwd changes, and interactive prompts inside the agent's own terminal do not. Improving this by extending the ACP spec (e.g., `shellOutput` / `commandLog` event types) is a multi-vendor standards effort with a long time horizon.

Meanwhile, a distinct user need has emerged that the ACP model does not serve:

> "I don't need multi-agent orchestration for this task. I have one or more coding CLIs (Claude Code, Codex, Kiro, or plain bash) and I want to drive them **directly** — full terminal, keyboard input, real-time output — from any device, with the session surviving my laptop."

Adjacent tools occupy parts of this space, each with a trade-off OAB can improve on:

| | Herdr | OpenDray | OAB PTY mode (proposed) |
|---|---|---|---|
| Session location | Your laptop | Host-resident server (bare metal / VM) | Remote K8s pod (sandboxed) |
| Laptop dies | Session dies | Session survives | Session survives |
| Blast radius | Large — full laptop access | Large — shared host credentials, SSH agent | Small — pod-isolated workspace |
| Coexists with message-broker mode | No | No | Yes — same binary, same deployment |

No existing tool offers **remote + sandboxed + full raw-terminal visibility**. That quadrant is empty, and OAB's existing session-pool and pod-sandbox architecture puts it one session backend away from filling it.

---

## 2. Decision

Add an opt-in **PTY mode** to the OpenAB unified binary: a second session backend where OAB spawns a CLI as a PTY subprocess inside its pod and streams the raw terminal bidirectionally to clients over WebSocket.

OAB becomes dual-persona:

```
Persona 1: "Agent Broker" (existing, default)
  - ACP stdio JSON-RPC, session pool
  - Input: Discord/Slack/Telegram messages (@mention, turn-based)
  - Output: structured platform replies
  - For: teams, async workflows, non-terminal users

Persona 2: "Terminal Server" (new, opt-in)
  - PTY subprocess, raw byte stream
  - Input: keyboard over WebSocket (character-level, real-time)
  - Output: terminal render (xterm.js / terminal widget)
  - For: developers who want direct, hands-on control
```

### Architecture

```
Clients
  ├── Web terminal (xterm.js, served from OAB admin surface)
  ├── Mobile / desktop app (terminal widget)
  └── CLI attach tool (future)
  │
  │── GET /pty/{session} (Upgrade: websocket, Bearer token)
  │── binary frames: raw PTY bytes (both directions)
  │── text frames: control messages (resize, ping, detach)
  │
OpenAB Unified Binary (in K8s pod)
  ├── PTY spawner (portable-pty crate: openpty + spawn)
  ├── Scrollback ring buffer (replayed on reattach)
  ├── Session pool ← extended with a PTY session backend
  │
  └── PTY subprocess inside the same sandbox:
        bash / claude / codex / kiro-cli / anything on PATH
```

### Interaction model differences

| | ACP mode (existing) | PTY mode (new) |
|---|---|---|
| Input | Platform message | stdin bytes (keyboard, incl. `y/n`, Ctrl+C) |
| Output | Structured reply | stdout/stderr stream, full scrollback |
| Timing | Async, turn-based | Synchronous, character-level |
| Client | Discord/Slack/TG | WebSocket terminal client |
| Visibility | Only what ACP emits | Everything the agent sees |

Messaging platforms cannot render a raw terminal, so PTY mode is **not** reachable from Discord/Slack — it requires a WebSocket terminal client. Both modes coexist in one deployment: a team can use ACP mode in Discord while a developer attaches a PTY session to the same pod.

### Session pool reuse

The existing pool already handles per-thread lifecycle, max sessions, idle TTL, eviction, and process-group kill handles. PTY mode adds a session *backend*, not a parallel pool:

- Key: `pty:{session_name}` instead of platform thread key
- Handle: PTY master fd + child pgid (kill/eviction machinery reuses existing pgid handling)
- Resume: reattach = replay scrollback buffer + resubscribe the stream (no `session/load` — the process never died)

### Security model

The sandbox posture is unchanged — this is the core differentiator versus OpenDray:

- PTY subprocess runs **inside the pod**, subject to the same isolation, `env_clear`, and workspace boundaries as ACP agents
- No host credentials, no host SSH agent, no host filesystem
- WebSocket attach requires Bearer token auth on upgrade (`OPENAB_PTY_AUTH_KEY`), independent from platform tokens
- Feature-gated and disabled by default: `OPENAB_PTY_ENABLED=true`

A raw terminal is a full shell inside the pod, so PTY mode grants strictly more capability to the *client* than ACP mode does. The trust boundary shifts from "agent decides what to run" to "the attached human decides." Token possession must therefore be treated as equivalent to pod shell access, and documented as such.

### Feature flag

```toml
[features]
pty = ["dep:portable-pty", "dep:axum"]
```

---

## 3. Consequences

### Positive

- **Fills an empty quadrant** — remote + sandboxed + raw-terminal visibility; safer than Herdr (not on your laptop) and OpenDray (not on a shared host), more visible than ACP mode
- **Session survives the client** — laptop dies, network drops: reconnect and replay scrollback; the pod keeps running
- **100% observability for hands-on work** — everything the CLI prints is visible, without waiting for ACP spec evolution
- **Reuses existing machinery** — session pool lifecycle, eviction, auth patterns, axum listener; PTY is a new backend, not a new system
- **Dual-persona, one deployment** — teams keep the Discord workflow; developers get direct control on the same infrastructure

### Negative

- **Two session models to maintain** — turn-based ACP and stream-based PTY have different lifecycle semantics (e.g., "idle" means something different when a human is attached)
- **Positioning overlap with OpenDray** — mitigated by the sandbox distinction, but the scopes now partially overlap
- **Expanded attack surface** — a WebSocket endpoint that hands out interactive shells demands careful token handling, rate limiting, and audit logging
- **Mobile UX ceiling** — character-level terminal interaction on a phone virtual keyboard is inherently worse than the message-based ACP flow

### Neutral

- ACP mode, platform adapters, and multi-agent dispatch are unaffected
- PTY sessions are invisible to messaging platforms by design; no notification bridge is included in the MVP (see Phase 4)

---

## 4. Alternatives Considered

### A. Extend ACP with observability events (deferred, complementary)

Push `shellOutput` / `commandLog` event types into the ACP spec so agents stream terminal content as structured events.

**Why deferred:** Requires spec evolution plus per-agent runtime adoption — a long, multi-vendor timeline outside OAB's control. Worth pursuing in parallel, but it does not deliver keyboard-driven direct control at all; it only improves ACP-mode visibility.

### B. Integrate with OpenDray instead (rejected)

Let OpenDray own PTY lifecycle; OAB handles platform routing; bridge the two.

**Why rejected:** Inherits OpenDray's host-resident security model, which contradicts OAB's mandatory sandbox principle. Adds a cross-project runtime dependency and a second deployment for a capability that is one crate away given OAB's existing session pool.

### C. Standalone PTY sidecar container (rejected for MVP)

Ship `openab-pty` as a separate sidecar to keep the core untouched.

**Why rejected for MVP:** Splits session management into two processes and creates a second security zone to reason about. The unified-binary + feature-flag pattern (see the ACP server ADR) already gives us opt-in isolation at compile time without runtime fragmentation. Revisit if PTY mode's dependency footprint grows.

### D. Keep ACP-only (rejected)

**Why rejected:** Leaves the "direct terminal control, remote, sandboxed" need unserved; users who want it must accept Herdr's laptop fragility or OpenDray's host blast radius.

---

## 5. Implementation Plan

### Phase 1: Core PTY backend (MVP)

- `portable-pty` spawner: named sessions running a configurable command (default `$SHELL`)
- `GET /pty/{session}` WebSocket upgrade on the existing axum listener; binary frames for PTY bytes, text frames for control (resize, detach)
- Bearer token auth on upgrade
- Scrollback ring buffer (configurable size) replayed on attach
- Session pool integration: max sessions, idle TTL (timer paused while a client is attached), pgid-based eviction
- Feature-gated behind `pty`, disabled by default

### Phase 2: Web terminal client

- Minimal xterm.js page served by OAB (attach, resize, reconnect)
- Session list/create/kill endpoints for the same surface

### Phase 3: Lifecycle hardening

- Reconnect with backoff and replay cursor
- Multiple concurrent viewers per session (one writer, N readers)
- Audit log of attach/detach events

### Phase 4: Optional messaging bridge

- Push a platform notification (Discord/Slack) when an unattached PTY session appears to be waiting on input — bridging back to OAB's messaging strength without rendering the terminal there

---

## 6. Prior Art Learnings

Techniques surveyed from adjacent projects, mapped to the implementation phases above.

### OpenDray (`internal/session/`, Go)

| Technique | What it does | Adopt in |
|---|---|---|
| Ring buffer with monotonic cursor (`ringbuf.go`) | The buffer tracks a monotonic `written` byte counter; clients pass their last offset as `since` on reconnect and receive only the missed bytes. If a client lags past the buffer capacity, the reply's `Start > since` gap explicitly reports how many bytes were dropped. | Phase 1 |
| Terminal-capability response filtering (`terminal_capabilities.go`) | xterm.js auto-answers CLI capability queries (Primary DA, CPR, Status Report); those escape sequences injected into stdin reliably break some Ink-based CLIs at startup. OpenDray strips well-formed answer patterns at the PTY boundary — one chokepoint protects every client emulator. | Phase 1 |
| Pure lifecycle state machine (`transitions.go`) | A side-effect-free `(State, Event)` transition table, exhaustively table-tested. Termination is split three ways — `user_stop`, `exit` (CLI died on its own), `gateway_shutdown` (daemon took the PTY down) — so post-restart reconciliation auto-resumes only the `interrupted` class. | Phase 3 |
| Server-side virtual terminal (`pump.go` + vt10x) | PTY output feeds a headless VT emulator in parallel with the ring buffer and live fanout, so the server can snapshot the post-ANSI rendered screen for notifications. Rust equivalents: `avt`, `vt100`. | Phase 4 |
| Idle detection → notification pipeline (`pump.go`) | Output marks activity; a watcher fires `session.idle` past a threshold and ships the last N lines from the ring buffer as the notification snippet. | Phase 4 |
| TUI chrome filtering (`claude_chrome.go`, `term.go`) | Conservative regex passes strip agent-CLI chrome (spinners, model bars, key hints) from screen snapshots so notifications read cleanly. False positives lose a line; they never delete real content. | Phase 4 |
| JSONL transcript as a second channel (`claude_jsonl.go`, `codex_jsonl.go`) | Reads the agent's own transcript files alongside the raw PTY — structured events without waiting for protocol evolution. | Future ADR |

### Herdr (Rust)

| Technique | What it does | Adopt in |
|---|---|---|
| Semantic agent state detection | Classifies each pane as `working` / `blocked` / `idle` / `done` from the PTY screen using per-agent detection manifests (TOML, remotely updatable, locally overridable), with an `agent.explain` API that reports which rule matched and why. `blocked` (waiting on approval) and `idle` (done) warrant different notifications. | Phase 4 |
| Race-safe waits | `agent.wait --until blocked/done` is server-owned and event-driven, and pins the resolved pane occupant so a replacement process cannot satisfy a stale wait; `agent.prompt` accepts an atomic `wait` object to eliminate the prompt-then-wait race. | Phase 1 API design |
| Layered restore taxonomy | Five distinct paths: live persistence (detach/reattach), live handoff (transfer live PTYs to a replacement server on upgrade), native agent session restore (integrations report the agent's own session ID; restart resumes via `claude --resume <id>` instead of replaying bytes), pane history replay (**off by default — scrollback may contain secrets**), and layout-only snapshot restore. | Phases 1/3; secrets-safe default adopted |
| Multiple read projections | `pane.read` offers `visible` (viewport), `recent` (scrollback), `recent-unwrapped` (logs, ignores soft wrap), and `detection` (state-detection snapshot) views of one PTY. | Phase 2/3 |
| Callback env injection | Spawned pane processes receive `HERDR_SOCKET_PATH` / `HERDR_PANE_ID`, so agents inside panes can drive the multiplexer — spawn peers, prompt each other, wait on each other. | Phase 3 |

### Claude Code cross-session messaging (v2.1.224+)

| Technique | What it does | Adopt in |
|---|---|---|
| Per-session UDS inbox + filesystem discovery | Each session binds a Unix socket restricted to the OS user and registers itself in files on disk; peers discover each other by reading those files. Filesystem visibility *is* the reachability boundary — container isolation falls out for free. | Future pod-internal multi-session messaging |
| Deliberately small message contract | Messages are plain-text summaries only, never conversation history or files; moving a conversation is the session-resume feature's job. Avoids context leakage, size blowups, and schema evolution. | A2A semantics for PTY-mode sessions |
| Permission-class trust model | Inbound messages can never approve permissions, change configuration, or execute embedded commands. The default deliver/hold decision derives from both sessions' permission-mode classes (bypassing vs prompting); held messages expire (default 5 min) and report back to the sender. | A2A semantics |
| Own-child verification, dual-track | A hook posting back to its own session's socket is verified by process evidence (peer credentials) where available, falling back to a per-session token sent as a first-line auth frame where it is not (macOS after process exit, PID-1 containers). | Phase 1 auth design |
| Message-storm prevention | Messages are read between tool calls (a running tool is never interrupted), rate-limited per sender, deduplicated within a short window, and capped at 50 queued — loops between two sessions stop on their own. | A2A semantics |

---

## 7. References

- [portable-pty crate](https://crates.io/crates/portable-pty) — cross-platform PTY handling (wezterm project)
- [xterm.js](https://xtermjs.org/) — browser terminal renderer
- [OpenDray](https://opendray.dev/) — host-resident PTY session persistence (prior art, different security model); `internal/session/` package
- [Herdr](https://herdr.dev/) — agent multiplexer with semantic state detection and socket API (prior art, laptop-local)
- [Claude Code cross-session messaging](https://code.claude.com/docs/en/cross-session-messaging) — UDS inbox, trust model, and loop throttling (prior art for A2A semantics)
- [ADR: ACP Server with WebSocket Transport](./acp-server-websocket.md) — shared axum listener and auth pattern
- `crates/openab-core/src/acp/pool.rs` — session pool to be extended with the PTY backend

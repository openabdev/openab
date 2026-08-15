# ADR: openab-pty — Composable Runtime for Remote Sandboxed Terminals

- **Status:** Proposed
- **Date:** 2026-08-15
- **Author:** @pahud
- **Related:** [ADR: ACP Server with WebSocket Transport (base, as-built)](./acp-server-websocket-base.md), [ADR: Separate Binaries with Opt-In Unified Build](./unified-binary.md), [ADR: Secrets Management](./secrets-management.md), [ADR: Identity Trust None](./identity-trust-none.md)
- **Supersedes:** the in-process "PTY Mode" proposal (PR #1477, closed) — group review verdict and rationale are preserved in that PR's consolidated review
- **Implementation:** TBD

---

## 1. Context & Problem

A distinct user need exists that OAB's ACP model does not serve:

> "I don't need multi-agent orchestration for this task. I have one or more coding CLIs (Claude Code, Codex, Kiro, or plain bash) and I want to drive them **directly** — full terminal, keyboard input, real-time output — from any device, with the session surviving my laptop."

Adjacent tools each carry a trade-off: Herdr is laptop-local (laptop dies, session dies), OpenDray is host-resident (shell shares host credentials). Cloud IDEs and managed web terminals (Codespaces, Cloud Shell) serve related needs but are vendor-hosted. A **self-hostable, K8s-pod-sandboxed** raw-terminal offering -- one that can live beside your ACP agents' workspace -- does not exist.

A previous proposal (PR #1477) made this a second in-process backend inside the OAB unified binary. Group review rejected that *form* — not the need — on five grounds:

1. **Positioning**: a terminal server inside the broker contradicts DESIGN.md pillar #1 ("thin bridge" as a deliberate non-decision)
2. **Blast radius**: a PTY shell co-resident with the broker shares its PID/cgroup/network namespaces and mounted credential plane; "sandbox posture unchanged" did not hold
3. **Auth**: a static shared key cannot carry pod-shell-equivalent trust
4. **Lifecycle**: the ACP session pool is turn-based and ACP-specific; PTY byte-stream liveness is incompatible, so "reuse the pool" was not an available boundary
5. **Reversibility**: absorbing a second product persona into one binary is hard to undo

This ADR proposes the same capability in a shape that answers all five.

---

## 2. Decision

Ship **`openab-pty`**: a separate binary that is an **independently runnable runtime** — deployable standalone or colocated with the OAB broker. Not deployed by default. OAB remains a pure ACP broker; `openab-pty` owns everything terminal.

**One codebase, two composable runtimes, three deployment modes:**

| Profile | Processes | Use case |
|---|---|---|
| 1. ACP only (current default) | `openab` | Message-broker deployments; no change from today |
| 2. PTY only | `openab-pty` | Standalone remote terminal service: workspace PVC + `[pty]` config + admin bootstrap credential; no Discord/Slack tokens, no platform adapters, no ACP protocol |
| 3. ACP + PTY (colocated) | `openab` + `openab-pty` sidecar | Both in one pod sharing the workspace volume: drive a CLI by hand, let ACP agents continue in the same working tree from Discord |

Deployment mechanics:

- **Own image**: `ghcr.io/openabdev/openab-pty` — smaller than the broker image (no platform adapter dependencies)
- **Own Service/Ingress**: `/pty/*` routes to the `openab-pty` port in both profile 2 and 3; the broker listener never serves terminal traffic
- **Helm UX**: independent toggles (`openab.enabled` / `pty.enabled`) or a convenience `--set profile=acp|pty|full`

```
Profile 3 (colocated) — K8s Pod
+--------------------------------------------------------------------+
|                                                                    |
|  Container: openab (broker)          Container: openab-pty         |
|  +---------------------------+       +---------------------------+ |
|  | ACP session pool          |       | PTY session manager (own) | |
|  | Platform adapters         |       | portable-pty spawner      | |
|  | Discord/Slack/... WS      |       | scrollback ring buffer    | |
|  |                           |       | GET /pty/{session} (WSS)  | |
|  | [own config view:         |       |                           | |
|  |  platform tokens, agents] |       | [own config view:         | |
|  +------------+--------------+       |  [pty] section only]      | |
|               |                      +-------------+-------------+ |
|               |   (colocated profile only, Phase 4)|               |
|               +<--- events (broker-initiated pull) +               |
|                                                                    |
|  Shared: workspace volume (PVC),  |   NOT shared: credentials,     |
|  pod network namespace, pod fate  |   PID/cgroup, filesystem mounts|
+--------------------------------------------------------------------+

Profile 2 (standalone) is the right half alone: openab-pty + workspace PVC.
```

### Positioning statement for the standalone profile

Profile 2 makes `openab-pty` a small standalone product in OpenDray's category (self-hosted persistent terminal sessions), differentiated by the K8s pod sandbox and the short-lived per-session token model. This is deliberate and bounded: `openab-pty` never grows platform adapters, agent orchestration, or memory features — users who need those deploy profile 3 and get them from the broker. This boundary is what keeps the OAB broker's thin-bridge identity untouched in every profile.

### Why a separate runtime (and what it fixes)

| Review blocker (PR #1477) | How the sidecar form resolves it |
|---|---|
| Positioning vs Thin Bridge | OAB binary is untouched; the broker stays a pure transport. `openab-pty` is an adjacent tool that shares deployment infrastructure only — no dual persona |
| Same-pod blast radius | Separate container = separate PID namespace, cgroup, filesystem, and mounts: the shell user cannot signal the broker, exhaust its container cgroup, or read its credential files. Broker platform tokens are **never mounted** into the PTY container. Residual sharing in the colocated profile (pod network namespace, pod fate) is graded honestly in Isolation tiers below; full isolation = profiles 1+2 as separate pods |
| Auth below capability | The sidecar designs its token model from scratch for shell-equivalent trust (see Security model) with no ACP-key coupling |
| Pool incompatibility | The sidecar has its **own session manager** built for byte-stream lifecycle. No refactor of the shipped ACP pool; zero regression risk to the broker |
| Reversibility | Default-off runtime with its own image/release. If demand does not materialize, deprecate the image; nothing in the broker to unwind. If demand proves out, later extraction of a shared lifecycle crate — or even single-process merge — remains open |

### Coexistence with ACP

ACP and PTY coexist per deployment, not per process:

- **Same pod, two containers (profile 3)** — one Helm toggle (`pty.enabled=true`) adds the sidecar; the broker container is byte-identical across all three profiles
- **Shared workspace volume (opt-in)** — the PTY shell and ACP agents can see the same working tree (same PVC mount), which is the practical point of coexistence: drive a CLI by hand in the terminal, then let ACP agents continue in the same workspace from Discord. This sharing is an **explicit cross-runtime trust and concurrency bridge**, stated rather than implied:
  - *Trust*: the workspace is a single trust zone. Workspace-resident credentials (`.git/credentials`, `.env` files, agent OAuth stores) are readable by the shell regardless of mount hygiene, and either side can plant content (hooks, PATH-shadowing binaries) the other later executes. Treat ACP and PTY principals as sharing workspace authority when sharing is enabled
  - *Concurrency*: concurrent writes are best-effort and uncoordinated (a runtime non-goal). Recommended convention: separate git worktrees or session directories per principal; document RWO/RWX PVC implications in the chart
  - PTY cannot attach to a running ACP agent subprocess — the runtimes share files, never processes
- **Separated in every profile**: credential mounts, PID namespaces, cgroups, filesystems, tokens, and session state
- **Shared in profile 3 (accepted residual risk)**: the pod network namespace (containers reach each other on localhost; Kubernetes NetworkPolicy selects pods, not containers, and cannot block intra-pod traffic), pod scheduling/restart fate, and pod-level resource pressure. "Independent failure domains" is therefore NOT a property of profile 3 -- it is a property of profiles 1+2

**Isolation tiers** (operators choose deliberately):

| Profile | Isolation tier |
|---|---|
| 1 + 2 as separate pods | Full: independent network identity, NetworkPolicy, failure domains -- **recommended for production when strong isolation is required** |
| 3 (colocated sidecar) | Partial: process/filesystem/credential-mount separation only; shared pod network and fate; the per-session token requirement and auth on every broker listener are the remaining intra-pod barriers. A convenience tier for teams that accept this trade for same-workspace ergonomics |

### Configuration: one source, two projected views

Operators keep a **single logical `config.toml`** (the existing `configUrl` flow); each runtime consumes a **projection materialized outside its trust boundary**. In the standalone profile (PTY only), the same file format applies -- `openab-pty` reads `[pty]` plus shared basics (workspace path, log level); no Discord/Slack tokens or ACP agent config are required or accepted.

- The broker reads its existing sections; it ignores `[pty]`
- The PTY runtime receives **only** a pre-filtered `[pty]` projection. Self-filtering a shared config is NOT an accepted secure delivery: `--section pty` limits parsing, not access -- if the PTY container holds the source URL and fetch credentials, a shell user can fetch the full broker config directly. The sanitized projection MUST be generated outside the PTY trust boundary (CI, chart, or operator tooling) and delivered via its own object/URL with a fetch identity scoped to that object only
- **Deployment contract -- the PTY container spec MUST NOT mount**: the ServiceAccount token (set `automountServiceAccountToken: false` at **pod** level -- it is not per-container -- and, when the broker needs IRSA/configUrl credentials in the colocated profile, project an audience-scoped token volume **into the broker container only**), the broker config or its source credentials, platform secrets, or any volume broader than the workspace (workspace-only volume or `subPath`; never the broker HOME PVC, which contains caches and session state)
- **Credential-material delivery (MUST)**: MVP has no signing key to deliver (see Token format). The only secret the PTY runtime holds is the **hash of the admin bootstrap credential**, and its delivery follows this rule: **external tooling (operator/Helm/CI) resolves any logical `aws-sm://` reference at deploy time and materializes the literal value into the delivered projection -- `openab-pty` itself never resolves cloud references at runtime and holds no cloud identity.** Delivered material is owned by the runtime UID, mode 0400, never enters the child's environment, logs, or core dumps (dumpable disabled)
- **Filesystem layout (MUST)** -- the layout is what enforces "never in the child's filesystem view" as an organizational contract, so it is specified, not implied:
  - `/run/openab-pty/` -- runtime-only directory holding the control socket and any runtime state; backed by a **dedicated writable tmpfs/emptyDir mount** (required: `readOnlyRootFilesystem: true` makes the root filesystem unwritable); created by the runtime at startup, **never** exported to the child (not in its environment, not under its HOME or cwd)
  - `/etc/openab-pty/` (read-only mount) -- the delivered config projection including the admin credential hash
  - the workspace volume -- the child's HOME and cwd, and the only writable **persistent** mount (the `/run/openab-pty` tmpfs is the sole other writable mount, and it is runtime-scoped and non-persistent)
  - **Scope of this boundary, stated honestly**: path separation is accident/organization prevention, not a kernel boundary -- a same-UID child in the same mount namespace can open these paths by absolute path. Confidentiality does not rest on filesystem invisibility; it rests on the hash-only design (nothing under these paths mints authority; see Same-container containment)
- **Same-container containment (MVP model)**: with the keyless token design, runtime **persistent state** contains no minting authority -- only non-reversible hashes. Stated precisely: the runtime *is* the mint, and freshly minted tokens and the presented admin credential transiently exist in its process memory; what the design removes is any *at-rest* stealable authority (no signing key, no stored plaintext). Protecting the transient window is therefore load-bearing: the runtime MUST set `PR_SET_DUMPABLE=0` **before any secret enters memory** -- this is what blocks same-UID `/proc/<pid>/mem`, `/proc/<pid>/fd`, and `ptrace` access on Linux; without it the containment claim does not hold. The remaining basis: the admin bootstrap credential is CSPRNG high-entropy and the runtime stores **only its verifier hash** (reading the hash neither authorizes requests nor enables practical offline guessing); every control operation is authenticated by presenting the credential itself, even on a locally reachable socket. **Optional hardening (not MVP)**: spawning PTY children under a distinct unprivileged UID gives defense-in-depth, but a non-root UID-1000 process cannot `setuid` without `CAP_SETUID` or a privileged launcher -- deployments that can provide a narrowly-scoped launcher may adopt it; the default container contract (non-root, all capabilities dropped, `allowPrivilegeEscalation: false`) is preserved either way
- **Same-UID residual-risk checklist (MUST)** -- because file modes are not a boundary between same-UID processes, the following are requirements, not recommendations:
  - Config projection and any secret material are delivered on **read-only mounts** (tamper-proofing comes from the mount, not the file mode)
  - Admin-credential verification is **constant-time** against the stored hash, and the presented credential buffer is **zeroized immediately after verification** (the same discipline as attach tokens)
  - Attach-token plaintext is **zeroized promptly after hashing**; only the hash is retained
  - The admin bootstrap credential is **generated, never operator-chosen**, with a minimum entropy of **256 bits** -- matching the attach-token strength, so the admin plane is never the weaker of the two credential planes
  - **Rotation, stated**: `session renew` rotates attach tokens, never the admin credential. Rotating the admin credential = generating a new value, updating the delivered hash, and restarting the runtime -- which clears all sessions. This is acceptable (sessions are non-persistent by contract) and is the documented procedure
  - `PR_SET_DUMPABLE=0` is a **Linux-specific** mitigation; non-Linux targets are out of scope for MVP and must not be assumed covered
  - **Accepted risk, stated**: a same-UID child can signal (including SIGKILL) the runtime process; this is availability, not confidentiality -- sessions die with the runtime and no credential is exposed by the crash
- **Resolution asymmetry (deliberate)**: the broker resolves `${VAR}` interpolation and `[secrets.refs]` cloud references itself, as today. The PTY runtime accepts only literal values, `${VAR}` environment interpolation, and local file paths in its delivered projection -- it MUST NOT link or invoke a cloud secrets resolver at runtime. A delivered PTY projection that still contains a `[secrets.refs]` table, any unresolved cloud reference (`aws-sm://` etc.), **or any `${secrets.*}` interpolation** is a **startup error** (fail closed) -- `${secrets.*}` is enumerated explicitly because it shares the `${}` delimiters with the accepted `${VAR}` env form and must never be silently treated as an unset environment variable. This guard prevents an implementer from re-importing a cloud fetch identity into the PTY trust boundary

```toml
# ---- LOGICAL operator source (what the operator maintains) ----
# Deploy tooling projects this into two delivered views; neither
# container ever receives the other's sections.

[secrets.refs]                      # broker view only. For the PTY projection, deploy
                                    # tooling resolves this at deploy time and writes the
                                    # literal value -- openab-pty never resolves aws-sm://
                                    # itself and holds no cloud identity
pty_admin_hash = "aws-sm://openab/pty-admin#hash"

[discord]                           # broker view only -- never delivered to the PTY runtime
bot_token = "${DISCORD_BOT_TOKEN}"

[agent]                             # broker view only
# ...

[pty]                               # PTY view only
enabled = true
listen = "0.0.0.0:8090"             # own port; TLS contract below
tls_terminated_upstream = false     # true only behind a trusted TLS-terminating Ingress
command = "/bin/bash"               # operator-configured; never client-specified
max_sessions = 4
absolute_session_ttl = "12h"        # applies even while attached
scrollback_kib = 1024               # in-memory only; cleared on teardown
scrollback_replay = false           # governs fresh-attach full-history dump only (see lifecycle)
admin_credential_hash = "${secrets.pty_admin_hash}"  # logical reference in the source only
```

```toml
# ---- DELIVERED PTY projection (what openab-pty actually receives) ----
# Generated outside the PTY trust boundary; contains no [secrets.refs],
# no cloud references, no ${secrets.*} interpolation, no broker sections.
# Anything else = startup error.

[pty]
enabled = true
listen = "0.0.0.0:8090"
tls_cert = "/etc/openab-pty/tls.crt"    # termination (a): in-process TLS. Off-loopback
tls_key  = "/etc/openab-pty/tls.key"    # bind without TLS material fails closed unless
tls_terminated_upstream = false         # tls_terminated_upstream = true (termination (b))
command = "/bin/bash"
max_sessions = 4
absolute_session_ttl = "12h"
scrollback_kib = 1024
scrollback_replay = false
admin_credential_hash = "sha256:9f2c..."  # literal verifier hash, materialized at deploy time.
                                          # SHA-256 suffices: the credential is generated
                                          # 256-bit CSPRNG, so memory-hard hashing (argon2)
                                          # adds nothing -- that defense targets low-entropy
                                          # human-chosen secrets, which are forbidden here
```

### Security model

- **Transport / TLS contract**: WSS is mandatory for external clients. Two supported terminations, chosen explicitly per deployment: (a) `openab-pty` terminates TLS itself (cert mounted into the container), or (b) a trusted Ingress terminates TLS and forwards plain WS internally -- in which case the internal listener accepts non-loopback plain WS only when the deployment declares `tls_terminated_upstream = true`, and the residual internal-hop exposure is documented. Fail-closed in all cases: the listener refuses to bind off-loopback without auth material configured (same guard the `/acp` endpoint enforces)
- **Browser credential transport**: reuse the validated `/acp` scheme -- `Authorization: Bearer` for non-browser clients, `Sec-WebSocket-Protocol: openab.bearer.<token>` for browsers (browsers cannot set the Authorization header on upgrade); constant-time comparison carries over. **Origin policy, decided explicitly (not "carried over")**: the as-built `/acp` consults `Origin` only on its keyless loopback path; its keyed bearer path never checks Origin -- and PTY has no keyless mode, so there is nothing to carry over. PTY's trust boundary is **bearer-only**: possession of a valid attach token is the sole authorization, and the `Origin` header is not consulted (it is attacker-controlled outside browsers and adds no strength to a keyed WebSocket). Browser-side token hygiene is governed by the client storage contract below
- **Token control plane** (MVP model; an identity layer remains explicitly out of scope per `identity-trust-none.md`):
  - **Create requires authentication -- and locality is not authentication**: the shell child may share the container (and potentially the UID) with the runtime, so a loopback/UDS endpoint alone is not an auth barrier. MVP mechanism: session creation (`openab-pty session create <name>`) requires an **admin bootstrap credential** delivered to the operator at deploy time and never present in the PTY child's environment or filesystem view. Hardening alternative (non-MVP): a distinct privileged UID/group for the runtime with UDS peer-credential verification. Unauthenticated remote create/list/kill is NOT provided in MVP; the admin credential and the attach token are distinct planes -- an attach token can never create, list, or kill
  - **Admin-credential delivery channels (MUST)** -- the containment model rests on the plaintext credential never being observable by a same-UID shell child, so every channel is enumerated and closed, not just env and files: the plaintext exists **only on the operator's trusted side** (their terminal/secret manager); in-container presentation accepts it **only via short-lived non-echoing stdin or a UDS message body**; it is **never** accepted as an argv flag (`/proc/<pid>/cmdline` is world-readable), never placed in any process environment inside the container, never written to temporary files, and never emitted to audit logs or errors (log a fingerprint of the verifier hash, not the credential). A same-UID shell observing one create/renew invocation must learn nothing reusable
  - **One-time issuance at creation**: creating a session mints an immutable `generation` and a fresh attach token, returned exactly once. **Issued once, not single-use**: the bearer token remains valid for reattach until its expiry or a generation bump -- reconnecting clients are not locked out; theft exposure is bounded by a short default TTL (well below the session TTL)
  - **Renewal (`openab-pty session renew <name>`)**: admin-authenticated like create; the session **process survives** (scrollback and state intact), the generation is bumped (all outstanding tokens for the session become invalid immediately), and a fresh attach token is returned exactly once. **Renew-while-attached, defined**: an actively attached connection is evicted via the standard evict order (notify, close with a distinct close code) before the fresh token is returned -- renew's use cases include suspected theft, so the possibly-hostile attached client must not linger under a revoked generation. Renew is distinct from **restart-in-place** (which replaces the process). MVP tokens are otherwise valid until expiry or kill; there is no client-side refresh on the attach surface
  - **Attach only verifies, never issues**: `GET /pty/{session}` validates the presented token; there is no minting path on the attach surface
  - **Per-session revocation**: kill/recreate bumps the generation and deletes the stored token hash, immediately invalidating outstanding tokens for that session; runtime restart clears all token state (sessions die with the process anyway, so this is not a loss)
  - **Token format (MVP): no signing key exists.** Each attach token is a CSPRNG 256-bit opaque bearer value; the runtime stores only its hash together with `(session ID, generation, scope = attach-only, expiry)` in memory and deletes it on kill/expiry. Because sessions deliberately do not survive a runtime restart, self-contained signed tokens buy nothing in MVP -- and eliminating the signing key eliminates the minting authority a same-container shell could steal. Signed (HMAC) tokens are a later option and require either an external signer outside the PTY container or the runtime/child privilege boundary below
- **Command authority**: the spawned command is operator configuration only; clients can never specify it. Session names are allowlist-validated (`[a-z0-9-]{1,32}`)
- **Isolation**: the PTY container mounts only the workspace volume (workspace-scoped, never the broker HOME PVC) and its own config projection; no service-account token, no broker config, no platform secrets (see the deployment contract above). NetworkPolicy applies at pod scope: it can restrict the standalone profile's pod independently; in the colocated profile it cannot separate the two containers (see Isolation tiers)
- **Container defaults**: the `openab-pty` image runs as a non-root user (UID 1000), `allowPrivilegeEscalation: false`, capabilities dropped, `readOnlyRootFilesystem` with the workspace as the only writable persistent mount plus the `/run/openab-pty` tmpfs (see Filesystem layout); child UID separation is optional hardening per Same-container containment above
- **Rate limiting in MVP**: per-IP WS-upgrade failure limits (e.g. 5 failures/min then a short ban) ship in Phase 1 -- audit is detection, rate limiting is prevention. **The admin control plane is throttled too, not just WS upgrades**: every admin-credential verifier (loopback/UDS `session create/renew/kill/restart` in Phase 1, remote admin endpoints in Phase 2) enforces a failure throttle with backoff, bounded request/body sizes, and a small concurrency cap on in-flight verifications (bounded work per attempt); admin auth failures are audited like attach failures
- **Client-side token storage (contract for the Phase 2 web client)**: the attach token is shell-equivalent, so the shipped client holds it **in memory only** -- never localStorage/sessionStorage, never in URLs (query strings leak via history, referrer, and proxy logs), never in cookies. Page reload = token gone = re-issue via renew, accepted UX. The served page sets a restrictive CSP; these rules are a Phase 2 acceptance criterion, stated now so the client is not designed around persistent storage
- **Audit in MVP**: attach/detach, session create/kill, and auth failures are logged from Phase 1; a leaked token must be observable
- **Env**: the PTY child gets an explicit allowlist (TERM, LANG/LC_*, PATH, HOME, USER, SHELL) and nothing else; `OPENAB_*` and cloud-credential variables are never inherited

### Session lifecycle (owned by `openab-pty`, designed for byte streams)

- **Liveness**: activity = client input OR PTY output OR a live attached socket (WS ping/pong at a 15-30s interval; a half-open socket counts as detached after 2-3 missed pings -- exact values are Phase 1 config with these recommended defaults, balancing flaky mobile networks against dead-client slot pinning)
- **TTLs**: detached-idle TTL (default 30m) plus an absolute session lifetime cap (default 12h) that applies even while attached -- capacity cannot be pinned forever by an open browser tab. Expiry is client-visible: a warning control frame precedes forced teardown, and the WebSocket closes with a distinct close code so clients surface "session expired" instead of retrying a network error
- **Attach semantics (MVP)**: single-attach exclusive, enforced by a session-level `owner_conn_generation` compare-and-swap: only the connection that wins the CAS holds the PTY write end; the replaced connection's write path is dropped before its socket closes, the PTY writer task honors only the current generation, and teardown of a replaced connection can never affect its successor. A second attach with a valid token takes over via this CAS (documented; multi-viewer is Phase 3). **Takeover abuse controls**: every successful preempt is audited as an anomaly event (session, source address, count), and preempt frequency is rate-limited per session (e.g. max N takeovers/min, then attaches are rejected with a distinct close code) -- a stolen still-valid token must not be able to silently ping-pong the CAS and starve the legitimate client; the audit trail plus `session renew` is the recovery path. **Ordering contract**: token revocation (generation bump + stored-hash deletion), the attach CAS, and replay registration execute under one session lock in that total order -- no interleaving where a revoked token wins an attach or a replay registers against a stale generation
- **Reconnect**: monotonic byte cursor from day one -- the ring buffer tracks total bytes written; clients reconnect with `since=<offset>` and receive only missed bytes. The replay-to-live handoff is **atomic**: the subscriber registers under the buffer lock, captures the end offset, replays through it, then drains queued live bytes -- with connection-generation fencing so teardown of a replaced connection cannot affect its successor. On overflow the server sends an explicit `gap` control frame (bytes-dropped count) so the client can trigger a full clear/redraw instead of rendering a sliced ANSI stream
- **Output-path bounds (MUST)**: every buffer on the PTY-to-client path is bounded, not just retained scrollback -- the replay-to-live handoff queue and the per-connection outbound backlog each have a fixed cap. A client too slow to drain its backlog gets a `gap` frame (drop-oldest, cursor advances) or, past a hard watermark, is disconnected with a distinct close code -- mirroring the input-side fail-closed backpressure so neither direction can grow unbounded memory
- **`scrollback_replay` vs cursor semantics** (distinct controls): incremental `since` replay is always available within the ring buffer's retention; `scrollback_replay` governs only the cursor-less full-history dump on a fresh attach (default off -- secrets-safe); setting `scrollback_kib = 0` disables retention entirely, which also disables `since` replay (every reconnect starts with a `gap` + reset)
- **Teardown**: setpgid on spawn; SIGTERM-grace-SIGKILL escalation on the process group; evict-while-attached order = notify client, close socket, kill group, close master fd, release slot; buffers cleared on teardown; scrollback never touches disk
- **Kill domain (MUST)**: the process group is only the first signal path, not the containment guarantee -- a child that calls `setsid` or double-forks escapes the pgid. **MVP default (works under the stated container contract, no extra capabilities): a pidfd-based descendant reaper** -- the runtime sets `PR_SET_CHILD_SUBREAPER` so escaped descendants reparent to it, holds a pidfd per tracked process, and performs race-safe descendant discovery before killing (re-scan until stable, then kill via pidfds). **The cgroup path (`cgroup.kill`, or freeze-then-kill) is the stronger boundary but is gated on an explicit prerequisite**: cgroup v2 with subtree delegation to the container's UID, which the default non-root/no-capabilities contract does not provide -- deployments that arrange delegation SHOULD prefer it. **Startup probe, fail closed**: at startup the runtime verifies its configured kill mechanism is operational (subreaper flag set and pidfd support, or a writable delegated cgroup subtree) and refuses to serve sessions otherwise. Slot release and the absolute TTL are enforced against this hard boundary, never against the pgid alone
- **Recovery taxonomy** (stated, not implied): detach/reattach survives (process alive); pod restart does not (process dead) -- reattach-to-dead returns a distinct error and offers **restart-in-place**: same session name, a fresh process and a new generation (old tokens invalid, empty scrollback). Pod-lifetime durability is out of scope and documented as such

---

## 3. Consequences

### Positive

- OAB keeps its thin-broker identity untouched — zero changes to the shipped binary, pool, or ACP path
- Fills the remote + sandboxed + raw-terminal quadrant with a real container boundary instead of a claimed one
- Highest reversibility: default-off, separately versioned, separately deprecable
- Coexistence where it matters (shared workspace) without shared process or credential-mount domains; network namespace and pod fate are shared only in the colocated profile (see Isolation tiers)
- The Phase 4 notification bridge (broker pulls from the sidecar -> relays to Discord) later reconnects the feature to OAB's messaging strength without merging the runtimes

### Negative

- A second binary and image to build, test, and release (mitigated by the existing multi-binary workspace and release pipeline)
- Cross-container coordination (notification bridge, future shared-crate extraction) is more ceremony than in-process calls
- Some duplication with the ACP pool (capacity accounting, pgid kill) until a shared lifecycle crate is justified by real usage

### Neutral

- Deployment surface grows only for operators who opt in; everyone else sees no change
- Whether this graduates to a shared crate or a merged process is deliberately deferred until product demand is proven

---

## 4. Alternatives Considered

### A. In-process dual-persona backend (rejected — the PR #1477 proposal)

Rejected by unanimous group review: positioning conflict with the Thin Bridge pillar, same-pod blast radius, auth/lifecycle mismatch, low reversibility. See the consolidated review on PR #1477.

### B. Extend ACP with observability events (deferred, complementary)

`shellOutput`/`commandLog` ACP events would improve in-bridge visibility for every client, but deliver no keyboard-level control. Worth pursuing independently; the JSONL-transcript idea from the prior-art survey belongs to that track, not this one.

### C. Integrate OpenDray / front a commodity tool (ttyd, gotty) (rejected for MVP)

Fronting ttyd/gotty against an OAB-managed pod delivers raw PTY-over-WS cheaply, but: no session-token minting, no scrollback-cursor reconnect contract, no lifecycle TTLs, no audit — the hardening this ADR requires would have to be built around the commodity core anyway, in a codebase we do not control. OpenDray integration inherits its host-resident model. Revisit if MVP scope proves too costly.

### D. `kubectl exec` + tmux runbook (rejected as the product answer)

Zero code and genuinely useful for cluster admins — but it requires kubectl credentials and cluster access, which is precisely what the target user (a developer on a phone or borrowed laptop) does not have. Documented as an operator escape hatch, not the product.

### E. Do nothing / remain ACP-only (rejected)

Leaves the need unserved; users accept Herdr's laptop fragility or OpenDray's host blast radius. The sidecar form lets OAB serve it without betting the broker's identity.

---

## 5. Implementation Plan

### Phase 1: `openab-pty` MVP (new crate, new binary)

- Own session manager: named sessions, operator-configured command, allowlist-validated names
- **Session bootstrap**: sessions are created via the authenticated loopback/UDS operator CLI (`openab-pty session create <name>`; `session renew <name>` re-issues a token per the token control plane; `session restart <name>` performs restart-in-place for reattach-to-dead per the recovery taxonomy), which spawns the PTY and returns the one-time attach token; `GET /pty/{session}` is attach-only. No remote create/list/kill in Phase 1 (Phase 2 adds them behind admin auth)
- portable-pty spawner with setpgid, escalating kill, the teardown order above, **and the hard kill boundary per the Kill domain MUST** (pidfd descendant reaper with `PR_SET_CHILD_SUBREAPER` as the default; delegated-cgroup kill where available; fail-closed startup probe)
- `GET /pty/{session}` WSS endpoint: binary frames = PTY bytes; text frames = versioned control schema (`resize`, `ping`, `detach`, `gap`, `ttl-warning`) with a defined close-code table. Frame validation is strict allowlist: bounded max frame size, unknown control types rejected, resize values bounds-checked; malformed frames count toward an abuse metric and can disconnect
- Input backpressure: per-connection write watermark toward the PTY master; a client exceeding it is disconnected (fail closed) rather than growing unbounded queues or stalling the reader
- Auth: the token control plane above (authenticated create, one-time issuance bound to session generation, attach-only verification); fail-closed off-loopback; `/acp`-style browser subprotocol transport; per-IP upgrade-failure rate limiting
- Monotonic cursor reconnect with atomic replay/live handoff and gap signaling; scrollback in-memory, off-by-default fresh-attach replay, cleared on teardown
- Detached-idle TTL + absolute lifetime cap (with client-visible expiry warning + close code); single-attach exclusive
- Audit log (attach/detach/create/kill/auth-failure) and basic metrics
- Resize propagation (TIOCSWINSZ) including attach-time initial size
- Terminal-capability response filtering at the PTY boundary (known Ink-CLI startup breakage)

### Phase 2: Deployment + web client

- Helm: independent `openab.enabled` / `pty.enabled` toggles (or `--set profile=acp|pty|full`); standalone profile gets its own Service/Ingress (`/pty/*`) and NetworkPolicy example; config split documented per the configUrl pattern; `ghcr.io/openabdev/openab-pty` image published from the existing release pipeline
- Minimal xterm.js page served by `openab-pty`; remote session list/create/kill endpoints gated by the **admin bootstrap credential** (the same control-plane contract as the Phase 1 CLI -- never attach tokens; attach-token scope remains attach-only)
- Rollback contract (Phase 2 acceptance criterion): disabling the toggle drains (notify + grace, honoring `terminationGracePeriodSeconds`) then kills sessions; the broker container is unaffected and not restarted. Projection updates roll the PTY container only -- live sessions die on the rollout (non-persistent by contract) and the chart documents this; the workspace PVC is untouched by disable/re-enable, and re-enable is a fresh runtime with zero sessions
- Projection tooling has an owner (Phase 2 acceptance criterion): the Helm chart generates both config views, and CI runs a **guard test** -- the delivered PTY projection is fed to `openab-pty`'s startup validator, which must accept it and must reject a deliberately poisoned projection (embedded `[secrets.refs]`, `${secrets.*}`, or a broker section). The "never delivered" invariant is thereby enforced by mechanism, not operator discipline

### Phase 3: Lifecycle hardening

- Multi-viewer (one writer, N readers) with writer-lease semantics and read-only token scope
- Reconnect backoff, richer capacity controls (per-token limits)

### Phase 4: Messaging bridge (optional, colocated profile only)

- `openab-pty` exposes a pod-local, loopback-only notification stream; the **broker pulls** (long-poll/SSE on localhost) when a detached session emits no output for N seconds after a prompt-like burst (stated heuristic, not magic); the broker relays to the platform thread. Bridge is one-way and feature-gated
- **No bridge secret enters the PTY container -- by design, stated now**: a delivered HMAC key would recreate exactly the in-container authority the keyless token model eliminated (a same-UID child can read any file the runtime can read; 0400 at the same UID is not a boundary). The pull model removes the broker-side ingress entirely: there is no webhook endpoint to leave open and no shared key to steal. Residual risk, stated: a same-UID child that kills the runtime (an accepted same-UID risk) could bind the freed port and forge events -- therefore the broker treats bridge events as **display-only, rate-limited hints**: they never carry commands, never mutate broker state, and are labeled best-effort in the relayed message. A push/webhook variant with an HMAC secret is permitted only with an external signer outside the PTY container or the runtime/child privilege boundary from the Security model (non-MVP hardening)
- **Not available in the PTY-only profile** — there is no broker to relay through, and `openab-pty` will not grow its own notifier (that would recreate the scope creep this ADR exists to avoid). Users who want notifications deploy profile 3
- **Pull-stream resource contract**: the notification stream is bounded like every other surface -- at most **one** concurrent broker stream (a new connection preempts the old), heartbeat with an idle timeout on both ends, reconnect with capped exponential backoff on the broker side, and a fixed-size event queue in `openab-pty` with coalescing (per-session dedupe: a newer idle event replaces an older undelivered one) and drop-oldest overflow. "Rate-limited hints" thus constrains retained resources, not only delivery semantics

### Later (demand-gated, explicitly deferred)

- Shared lifecycle crate extraction (if the ACP pool and PTY manager converge naturally). Candidate shared surface: spawn mechanics, env-allowlist construction, pgid kill/escalation; deliberately NOT shared: liveness definitions, TTL/eviction policy, persistence
- **Adoption review point**: 12 months after the standalone profile ships, review its usage; below a threshold the maintainers set then, consider deprecating the standalone image or folding PTY back to colocate-only
- Single-process merge (only if operations prove the sidecar split is more cost than benefit)
- Identity layer for PTY tokens; semantic agent-state detection; JSONL transcript channel (see Alternative B)

---

## 6. Prior Art Learnings

The full survey from the superseded proposal carries over unchanged in substance; the adopt-in targets below are normalized against Section 5 of this ADR.

### OpenDray (`internal/session/`, Go)

| Technique | What it does | Adopt in |
|---|---|---|
| Ring buffer with monotonic cursor (`ringbuf.go`) | Monotonic `written` byte counter; clients pass `since` on reconnect and receive only missed bytes; lag past capacity is reported explicitly as a gap | Phase 1 |
| Terminal-capability response filtering (`terminal_capabilities.go`) | Strips xterm.js auto-answers (DA/CPR/Status) from stdin at the PTY boundary; one chokepoint protects every client emulator from Ink-CLI startup breakage | Phase 1 |
| Pure lifecycle state machine (`transitions.go`) | Side-effect-free `(State, Event)` table; termination split into user-stop / self-exit / runtime-shutdown so restart reconciliation targets only the interrupted class | Phase 3 |
| Server-side virtual terminal (`pump.go` + vt10x) | PTY output feeds a headless VT emulator so notifications can snapshot the post-ANSI screen (Rust: `avt`, `vt100`) | Phase 4 |
| Idle detection -> notification pipeline (`pump.go`) | Output marks activity; a watcher fires an idle event with the last N lines as snippet | Phase 4 |
| TUI chrome filtering (`claude_chrome.go`, `term.go`) | Conservative regexes strip spinner/model-bar noise from notification snapshots | Phase 4 |
| JSONL transcript as a second channel (`claude_jsonl.go`) | Reads the agent's own transcript files as a structured side channel | Alternative B track |

### Herdr (Rust)

| Technique | What it does | Adopt in |
|---|---|---|
| Semantic agent state detection | Per-agent detection manifests classify panes as working/blocked/idle/done, with an explain API for rule provenance | Later (demand-gated) |
| Race-safe waits | Server-owned event-driven waits pinned to the pane occupant; atomic prompt+wait | Later (demand-gated) |
| Layered restore taxonomy | Live persistence / live handoff / native session restore / history replay (off by default: secrets) / layout-only snapshot | Phase 1 adopts the secrets-safe default and the recovery taxonomy |
| Multiple read projections | `visible` / `recent` / `recent-unwrapped` / `detection` views of one PTY | Phase 2/3 |
| Callback env injection | Spawned processes receive the runtime's socket path so in-pane agents can drive it | Later (demand-gated; A2A needs its own ADR) |

### Claude Code cross-session messaging (v2.1.224+)

| Technique | What it does | Adopt in |
|---|---|---|
| Per-session UDS inbox + filesystem discovery | Reachability boundary = filesystem visibility; container isolation falls out for free | Future A2A ADR |
| Deliberately small message contract | Plain-text summaries only, never history or files | Future A2A ADR |
| Permission-class trust model | Inbound messages cannot approve, reconfigure, or execute; deliver/hold derived from both sides' permission classes | Future A2A ADR |
| Own-child verification, dual-track | Process evidence where available, per-session token as first-line auth frame where not | Informs Phase 1 token design |
| Message-storm prevention | Read between turns, per-sender rate limits, dedupe, queue caps | Future A2A ADR |

---

## 7. References

- [PR #1477](https://github.com/openabdev/openab/pull/1477) — superseded in-process proposal; consolidated group-review rationale
- [portable-pty crate](https://crates.io/crates/portable-pty) — cross-platform PTY handling (wezterm project)
- [xterm.js](https://xtermjs.org/) — browser terminal renderer
- [OpenDray](https://opendray.dev/) — host-resident PTY session persistence (prior art, different security model)
- [Herdr](https://herdr.dev/) — agent multiplexer with semantic state detection (prior art, laptop-local)
- [Claude Code cross-session messaging](https://code.claude.com/docs/en/cross-session-messaging) — UDS inbox, trust model, loop throttling
- [ADR: ACP Server WebSocket (base)](./acp-server-websocket-base.md) — validated browser bearer-subprotocol auth and fail-closed listener guard reused here
- [ADR: configUrl over Helm rendering](./configurl-over-helm-rendering.md) — the config delivery pattern the two-view split builds on
- `docs/agentcore.md` — AgentCore's uVM PTY path; **non-goal boundary**: AgentCore runs *agents* in remote PTYs under its own runtime; `openab-pty` gives a *human* a terminal in the OAB workspace pod. Use AgentCore when you want managed agent execution; use `openab-pty` when you want hands-on control beside your ACP agents

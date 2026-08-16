# ADR: openab-pty — Composable Runtime for Remote Sandboxed Terminals

- **Status:** Proposed
- **Date:** 2026-08-15 (revised 2026-08-16 after the feasibility spike)
- **Author:** @pahud
- **Related:** [ADR: ACP Server with WebSocket Transport (base, as-built)](./acp-server-websocket-base.md), [ADR: Separate Binaries with Opt-In Unified Build](./unified-binary.md), [ADR: Secrets Management](./secrets-management.md) (applies to the deploy-time materialization of the admin credential hash only -- no PTY token-signing key exists in MVP), [ADR: Identity Trust None](./identity-trust-none.md)
- **Supersedes:** the in-process "PTY Mode" proposal (PR #1477, closed) — group review verdict and rationale are preserved in that PR's consolidated review
- **Implementation:** TBD — gated on the demand check in Section 5

> **Revision 2026-08-16.** The feasibility spike required by Section 5 is complete, and its measurements changed four decisions: the kill domain became **two tiers with startup detection** instead of a delegated-cgroup prerequisite (the hard guarantee needs a privileged component this ADR declines to require for MVP); the admin plane became **remote-only**, which satisfies admin-plane-unreachability by construction and deleted the entire in-container credential-channel requirement set; the spawn primitive corrected to a pre-`execv` pid write because `clone3(CLONE_INTO_CGROUP)` is filtered under seccomp `RuntimeDefault`; and substrate/adjacency coverage was broadened (RWX is *not* required — see the adjacency mechanisms). The spike's finding that ancestry cannot attribute sessions **stands**; what changed is which guarantee MVP buys. Evidence and the decision record are on the tracking issue.

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

Ship **`openab-pty`**: a separate binary that is an **independently runnable runtime** — deployable standalone or colocated with the OAB broker. Not deployed by default. OAB remains a pure ACP broker; `openab-pty` owns everything terminal. **MVP deploys it standalone only (separate pods, profiles 1+2); colocation (profile 3) is a demand-gated later profile.**

**One codebase, two composable runtimes, three deployment profiles — MVP ships two:**

| Profile | Processes | Use case |
|---|---|---|
| 1. ACP only (current default) | `openab` | Message-broker deployments; no change from today |
| 2. PTY only | `openab-pty` | Standalone remote terminal service: workspace PVC + `[pty]` config + admin bootstrap credential; no Discord/Slack tokens, no platform adapters, no ACP protocol |
| 3. ACP + PTY (colocated) — **demand-gated, not in MVP** | `openab` + `openab-pty` sidecar | Both in one pod sharing the workspace volume: drive a CLI by hand, let ACP agents continue in the same working tree from Discord |

**MVP scope (decision): profiles 1 and 2 only — always separate pods, the full-isolation tier.** The colocated profile 3 is design-recorded in this ADR (it is the shape that answers the #1477 coexistence question) but is **demand-gated**. The operative reason is no longer the isolation tier: **adjacency is achievable without colocation** (see the mechanisms below), so profile 3 buys nothing MVP needs. Everything colocated-specific below — the sidecar diagram, the partial-isolation tier, the Phase 4 notification bridge — is therefore a **target design recorded for that gate, not an MVP commitment**.

### Adjacency: how the workspace is shared, per substrate

The product's distinguishing property is a terminal **beside your ACP agents' workspace** — structurally unavailable to host-resident or laptop-local alternatives. Three mechanisms deliver it, and MVP uses the first two:

| Mechanism | Storage required | Isolation tier | Constraint |
|---|---|---|---|
| **A.** RWX volume across two pods | shared-filesystem class (EFS/NFS/CephFS/Azure Files) | **Full** — pods may sit on different nodes | needs a shared-FS storage class |
| **B.** RWO volume, two pods pinned to one node via pod affinity | **any block storage**, including the k3s `local-path` default and EBS | **Full** — separate network namespaces, NetworkPolicy selects each pod, independent restarts | scheduling pinned to one node; shared *node* fate |
| **C.** Colocated, one pod, two containers | anything, including `emptyDir` | **Partial** — shared pod network and pod fate; NetworkPolicy cannot select a container | the demand-gated profile 3 |

Mechanism **B** is why RWX is not required: per the Kubernetes access-mode definition, `ReadWriteOnce` restricts the volume to a single *node*, and **multiple pods on that node may all read and write it** (`ReadWriteOncePod` is the mode that restricts to one pod). B therefore obtains colocation's practical benefit on ordinary block storage while keeping the full-isolation tier, sharing only node fate.

**Substrate matrix** (the runtime detects its situation at startup and logs the active tier):

| Substrate | Attribution boundary | Adjacency | Extra infrastructure |
|---|---|---|---|
| Kubernetes, shared-FS storage | Tier 1 default; Tier 2 where delegation is arranged | A — two pods, RWX volume | shared-FS storage class |
| Kubernetes, block storage (incl. k3s `local-path`) | same | B — two pods pinned to one node | **none** |
| systemd host | Tier 2 native via `Delegate=yes` | shared directory | **none** |
| ECS Fargate | task boundary | two containers in one task sharing task ephemeral storage | **none** |
| ECS Fargate + EFS | task-per-session becomes available (task ID *is* the session ID; teardown is `StopTask`) | many tasks share EFS | EFS |

Three of these require no additional infrastructure, matching how OAB is deployed today. **Chart and task-definition requirements** that follow from this and are easy to get wrong:

- **Never declare the workspace PVC `ReadWriteOncePod`** — it is the one mode that genuinely blocks the second pod, leaving it `Pending` indefinitely. It sounds safer, which is precisely the trap.
- Access modes are matching/attachment declarations, **not enforcement**; read-only behaviour comes from `volumeMounts[].readOnly: true`.
- On ECS, use **explicitly named volumes with per-container `mountPoints`, never `volumesFrom`** — the latter shares *all* volumes of the source container and so violates the workspace-only mount contract below.
- **Align image UIDs** on any shared workspace mount. File ownership is real, and the broker images do not all run as the same user; EFS access points can force a POSIX identity, but task ephemeral bind mounts and most block volumes cannot.
- On ECS, note that **task-per-session and task-ephemeral sharing are mutually exclusive** — separate session tasks cannot see a task-scoped volume, so that shape requires EFS.

Deployment mechanics:

- **Own image**: `ghcr.io/openabdev/openab-pty` — smaller than the broker image (no platform adapter dependencies)
- **Own Service/Ingress**: `/pty/*` routes to the `openab-pty` port in both profile 2 and 3; the broker listener never serves terminal traffic
- **Helm UX (MVP)**: independent toggles (`openab.enabled` / `pty.enabled`) rendering **separate pods** — convenience form `--set profile=acp|pty`. The colocated `full` profile is **not rendered by the MVP chart**; it ships (opt-in only, never default) only if the profile-3 demand gate opens

```
Profile 3 (colocated; demand-gated target design — not in MVP) — K8s Pod
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

### Positioning: adjacency first

**The product is a terminal in the pod beside your ACP agents' workspace.** That is the property no adjacent tool can have: host-resident alternatives put the shell on your host with your host credentials, and laptop-local ones die with the laptop; neither sits next to an agent fleet in a shared working tree. What differentiates `openab-pty` is therefore its **deployment and credential model** — a locked-down pod (no service-account token, no broker config, no platform secrets, workspace-only mount, non-root, capabilities dropped, read-only rootfs) plus keyless short-lived per-session tokens — not terminal features.

Consequences of stating this honestly:

- Profile 2 standalone still ships from the same binary, but it is **not the headline**, and this ADR does not commit to matching mature terminal servers on terminal experience (ring-buffer polish, VT emulation, chrome filtering, and lifecycle state machines are things Section 6 plans to *adopt from* prior art). The 12-month adoption review already anticipates standalone failing to hold on its own.
- If demand later proves users want the standalone remote terminal *itself*, Alternative C (wrap a commodity terminal core and put the pod boundary and token model around it) becomes materially more attractive than it looked when written — precisely because the differentiation is the deployment and credential model rather than the terminal.

`openab-pty` never grows platform adapters, agent orchestration, or memory features; users who need those get them from the broker. That boundary is what keeps the OAB broker's thin-bridge identity untouched in every profile.

### Why a separate runtime (and what it fixes)

| Review blocker (PR #1477) | How the composable-runtime form resolves it |
|---|---|
| Positioning vs Thin Bridge | OAB binary is untouched; the broker stays a pure transport. `openab-pty` is an adjacent tool that shares deployment infrastructure only — no dual persona |
| Same-pod blast radius | Separate container = separate PID namespace, cgroup, filesystem, and mounts: the shell user cannot signal the broker, exhaust its container cgroup, or read its credential files. Broker platform tokens are **never mounted** into the PTY container. Residual sharing in the colocated profile (pod network namespace, pod fate) is graded honestly in Isolation tiers below; full isolation = profiles 1+2 as separate pods, **which is all MVP ships** |
| Auth below capability | `openab-pty` designs its token model from scratch for shell-equivalent trust (see Security model) with no ACP-key coupling |
| Pool incompatibility | `openab-pty` has its **own session manager** built for byte-stream lifecycle. No refactor of the shipped ACP pool; zero regression risk to the broker |
| Reversibility | Default-off runtime with its own image/release. If demand does not materialize, deprecate the image; nothing in the broker to unwind. If demand proves out, later extraction of a shared lifecycle crate — or even single-process merge — remains open |

### Coexistence with ACP

ACP and PTY coexist per deployment, not per process. **In MVP, coexistence means two separate pods**, sharing the workspace by mechanism A or B above; the same-pod form below is the demand-gated profile-3 target design:

- **Same pod, two containers (profile 3, demand-gated)** — one Helm toggle (`pty.enabled=true`) adds the sidecar; the broker container is byte-identical across all three profiles
- **Shared workspace volume (opt-in)** — the PTY shell and ACP agents can see the same working tree (same PVC mount), which is the practical point of coexistence: drive a CLI by hand in the terminal, then let ACP agents continue in the same workspace from Discord. This sharing is an **explicit cross-runtime trust and concurrency bridge**, stated rather than implied:
  - *Trust*: the workspace is a single trust zone. Workspace-resident credentials (`.git/credentials`, `.env` files, agent OAuth stores) are readable by the shell regardless of mount hygiene, and either side can plant content (hooks, PATH-shadowing binaries) the other later executes. Treat ACP and PTY principals as sharing workspace authority when sharing is enabled
  - *Concurrency*: concurrent writes are best-effort and uncoordinated (a runtime non-goal). Recommended convention: separate git worktrees or session directories per principal; document RWO/RWX PVC implications in the chart
  - PTY cannot attach to a running ACP agent subprocess — the runtimes share files, never processes
- **Separated in every profile**: credential mounts, PID namespaces, cgroups, filesystems, tokens, and session state
- **Shared in profile 3 (accepted residual risk)**: the pod network namespace (containers reach each other on localhost; Kubernetes NetworkPolicy selects pods, not containers, and cannot block intra-pod traffic), pod scheduling/restart fate, and pod-level resource pressure. "Independent failure domains" is therefore NOT a property of profile 3 -- it is a property of profiles 1+2

**Isolation tiers** (operators choose deliberately):

| Profile | Isolation tier |
|---|---|
| 1 + 2 as separate pods (**the MVP scope**) | Full: independent network identity, NetworkPolicy, failure domains -- **recommended for production when strong isolation is required** |
| 3 (colocated sidecar, **demand-gated — not in MVP**) | Partial: process/filesystem/credential-mount separation only; shared pod network and fate; the per-session token requirement and auth on every broker listener are the remaining intra-pod barriers. A convenience tier for teams that accept this trade for same-workspace ergonomics |

### Configuration: one source, two projected views

Operators keep a **single logical `config.toml`** (the existing `configUrl` flow); each runtime consumes a **projection materialized outside its trust boundary**. In the standalone profile (PTY only), the same file format applies -- `openab-pty` reads `[pty]` plus shared basics (workspace path, log level); no Discord/Slack tokens or ACP agent config are required or accepted.

- The broker reads its existing sections; it ignores `[pty]`
- The PTY runtime receives **only** a pre-filtered `[pty]` projection. Self-filtering a shared config is NOT an accepted secure delivery: `--section pty` limits parsing, not access -- if the PTY container holds the source URL and fetch credentials, a shell user can fetch the full broker config directly. The sanitized projection MUST be generated outside the PTY trust boundary (CI, chart, or operator tooling) and delivered via its own object/URL with a fetch identity scoped to that object only
- **Deployment contract -- the PTY container spec MUST NOT mount**: the ServiceAccount token (set `automountServiceAccountToken: false` at **pod** level -- it is not per-container -- and, when the broker needs IRSA/configUrl credentials in the colocated profile, project an audience-scoped token volume **into the broker container only**), the broker config or its source credentials, platform secrets, or any volume broader than the workspace (workspace-only volume or `subPath`; never the broker HOME PVC, which contains caches and session state)
- **Credential-material delivery (MUST)**: MVP has no **token-signing key** to deliver (see Token format), and in the same-UID MVP no TLS private key enters the container either (Ingress-terminated TLS is the default -- see the Transport/TLS contract). The only secret the PTY runtime holds is the **hash of the admin bootstrap credential**, and its delivery follows this rule: **external tooling (operator/Helm/CI) resolves any logical `aws-sm://` reference at deploy time and materializes the literal value into the delivered projection -- `openab-pty` itself never resolves cloud references at runtime and holds no cloud identity.** Delivered material is owned by the runtime UID, mode 0400, never enters the child's environment, logs, or core dumps (dumpable disabled)
- **Filesystem layout (MUST)** -- the layout is what enforces "never in the child's filesystem view" as an organizational contract, so it is specified, not implied:
  - `/run/openab-pty/` -- runtime-only directory holding the control socket and any runtime state; backed by a **dedicated writable tmpfs/emptyDir mount** (required: `readOnlyRootFilesystem: true` makes the root filesystem unwritable); created by the runtime at startup, **never** exported to the child (not in its environment, not under its HOME or cwd)
  - `/etc/openab-pty/` (read-only mount) -- the delivered config projection including the admin credential hash
  - the workspace volume -- the child's HOME and cwd, and the only writable **persistent** mount (the `/run/openab-pty` tmpfs is the sole other writable mount, and it is runtime-scoped and non-persistent)
  - **Scope of this boundary, stated honestly**: path separation is accident/organization prevention, not a kernel boundary -- a same-UID child in the same mount namespace can open these paths by absolute path. Confidentiality does not rest on filesystem invisibility; it rests on **no readable authority existing under these paths at all**: the config projection holds only the non-reversible verifier hash, and no TLS private key is present in the same-UID MVP (Ingress-terminated TLS -- see the Transport/TLS contract). Any future mounted secret that *is* authority (a TLS key, an HMAC key) requires the privilege/mount-namespace boundary first
- **Same-container containment (MVP model)**:
  - **What the keyless design removes**: runtime **persistent state** contains no minting authority -- only non-reversible hashes. There is no signing key and no stored plaintext for a same-UID shell to steal at rest
  - **What remains, stated precisely**: the runtime *is* the mint -- freshly minted tokens and the presented admin credential transiently exist in its process memory
  - **Protecting the transient window is therefore load-bearing**: the runtime MUST set `PR_SET_DUMPABLE=0` **before any secret enters memory** -- this is what blocks same-UID `/proc/<pid>/mem`, `/proc/<pid>/fd`, and `ptrace` access on Linux; without it the containment claim does not hold, as measured (at dumpable=1 an attacker read the credential from both `/proc/<pid>/mem` and an inherited pipe via `/proc/<pid>/fd`; at dumpable=0 both were denied). **`prctl` failure is fail-closed**: if `PR_SET_DUMPABLE=0` cannot be set (seccomp policy, kernel restriction), the runtime refuses to serve *before reading any credential input* -- the containment model must never degrade silently. **Exactly one process in the container ever holds credential plaintext** -- the runtime -- because the admin plane is remote-only and no in-container helper exists (see the Security model)
  - **Credential strength basis**: the admin bootstrap credential is generated by a CSPRNG (cryptographically secure pseudorandom number generator) at high entropy, and the runtime stores **only its verifier hash** -- reading the hash neither authorizes requests nor enables practical offline guessing; every control operation is authenticated by presenting the credential itself
  - **Optional hardening (not MVP)**: spawning PTY children under a distinct unprivileged UID gives defense-in-depth, but a non-root UID-1000 process cannot `setuid` without `CAP_SETUID` or a privileged launcher -- deployments that can provide a narrowly-scoped launcher may adopt it; the default container contract (non-root, all capabilities dropped, `allowPrivilegeEscalation: false`) is preserved either way
- **Same-UID residual-risk checklist (MUST)** -- because file modes are not a boundary between same-UID processes, the following are requirements, not recommendations:
  - Config projection and any secret material are delivered on **read-only mounts** (tamper-proofing comes from the mount, not the file mode)
  - Admin-credential verification is **constant-time** against the stored hash, and the presented credential buffer is **zeroized immediately after verification** (the same discipline as attach tokens)
  - Attach-token plaintext is **zeroized promptly after hashing**; only the hash is retained
  - The admin bootstrap credential is **generated, never operator-chosen**, with a minimum entropy of **256 bits** -- matching the attach-token strength, so the admin plane is never the weaker of the two credential planes
  - **Rotation, stated**: `session renew` rotates attach tokens, never the admin credential. Rotating the admin credential = generating a new value, updating the delivered hash, and restarting the runtime -- which clears all sessions. This is acceptable (sessions are non-persistent by contract) and is the documented procedure
  - `PR_SET_DUMPABLE=0` is a **Linux-specific** mitigation; non-Linux targets are out of scope for MVP and must not be assumed covered
  - **Accepted risk, stated**: a same-UID child can signal (including SIGKILL) the runtime process; this is availability, not confidentiality -- sessions die with the runtime and no credential is exposed by the crash
- **Resolution asymmetry (deliberate)**: the broker resolves `${VAR}` interpolation and `[secrets.refs]` cloud references itself, as today. The PTY runtime accepts only literal values, `${VAR}` environment interpolation, and local file paths in its delivered projection -- it MUST NOT link or invoke a cloud secrets resolver at runtime. A delivered PTY projection that still contains a `[secrets.refs]` table, any unresolved cloud reference (`aws-sm://` etc.), **or any `${secrets.*}` interpolation** is a **startup error** (fail closed) -- `${secrets.*}` is enumerated explicitly because it shares the `${}` delimiters with the accepted `${VAR}` env form and must never be silently treated as an unset environment variable. This guard prevents an implementer from re-importing a cloud fetch identity into the PTY trust boundary. **The validator is also fail-closed on the verifier itself**: in the delivered projection, `admin_credential_hash` MUST be **literal-only, non-empty, and format-validated** (`sha256:` followed by exactly 64 lowercase hex characters) -- a plain `${VAR}` interpolation on this key, an empty value, or a malformed hash is a startup error, never a silently-accepted config (a fail-open here would let a poisoned projection disable admin auth). Both poisoning cases -- unresolved interpolation and malformed/empty hash -- are added to the Phase 2 CI guard test alongside the `[secrets.refs]` / `${secrets.*}` cases

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
tls_terminated_upstream = true      # MVP default: trusted Ingress terminates TLS.
                                    # In-process TLS (a mounted key) is gated on a real
                                    # runtime/child privilege boundary -- see Security model
command = "/bin/bash"               # operator-configured; never client-specified
max_sessions = 4
absolute_session_ttl = "12h"        # applies even while attached
scrollback_kib = 1024               # in-memory only; cleared on teardown
scrollback_replay = false           # governs fresh-attach full-history dump only (see lifecycle)
admin_credential_hash = "${secrets.pty_admin_hash}"  # logical reference in the source only
```

Deploy tooling resolves `pty_admin_hash` from `[secrets.refs]` (the `#hash` fragment is the JSON key inside the secret, per the secrets-management ADR's `#<json-key>` contract -- the stored value *is* the verifier hash) and writes the literal into the delivered projection:

```toml
# ---- DELIVERED PTY projection (what openab-pty actually receives) ----
# Generated outside the PTY trust boundary; contains no [secrets.refs],
# no cloud references, no ${secrets.*} interpolation, no broker sections.
# Anything else = startup error.

[pty]
enabled = true
listen = "0.0.0.0:8090"
tls_terminated_upstream = true            # trusted Ingress terminates TLS (MVP default);
                                          # no TLS private key exists in this container
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

> **Granularity note — target contracts**: the MUST inventories in this section and in Session lifecycle are **target contracts**: they bind the Phase 1 acceptance tests, and implementation evidence may revise them via PRs against this ADR (each revision recorded, never silent). Phase 1 includes extracting them into **`docs/pty-security-contract.md`** as given/when/assert acceptance criteria — after which this ADR records *decisions and rationale* and the contract document owns the *testable clauses*. This resolves the ADR-to-spec granularity drift deliberately rather than letting the two genres blur.

- **Transport / TLS contract** -- WSS is mandatory for external clients:
  - **MVP default -- termination (b), trusted Ingress**: a trusted Ingress terminates TLS and forwards plain WS internally. This is **the only supported mode while the runtime and PTY children share a UID**; the internal listener accepts non-loopback plain WS only when the deployment declares `tls_terminated_upstream = true`, and the residual internal-hop exposure is documented
  - **Termination (a), in-process TLS -- gated on a real boundary**: a mounted certificate key requires a runtime/child privilege or mount-namespace boundary first (the same gating as HMAC bridge secrets and signed tokens): a `tls.key` readable by a same-UID child is persistent, stealable authority
  - **Risk grading, stated so the two secrets are not conflated**: a stolen TLS private key is *transport* authority (endpoint impersonation/MITM for that deployment); the admin credential hash is a *non-reversible verifier* (grants nothing when read) -- which is exactly why the key must stay outside the same-UID container while the hash may live inside it
  - **Fail-closed in all cases**: the listener refuses to bind off-loopback without auth material configured (same guard the `/acp` endpoint enforces)
- **Browser credential transport**: reuse the validated `/acp` scheme -- `Authorization: Bearer` for non-browser clients, `Sec-WebSocket-Protocol: openab.bearer.<token>` for browsers (browsers cannot set the Authorization header on upgrade); constant-time comparison carries over. **Origin policy, decided explicitly (not "carried over")**: the as-built `/acp` consults `Origin` only on its keyless loopback path; its keyed bearer path never checks Origin -- and PTY has no keyless mode, so there is nothing to carry over. PTY's trust boundary is **bearer-only**: possession of a valid attach token is the sole authorization, and the `Origin` header is not consulted (it is attacker-controlled outside browsers and adds no strength to a keyed WebSocket). Browser-side token hygiene is governed by the client storage contract below
- **Token control plane** (MVP model; an identity layer remains explicitly out of scope per `identity-trust-none.md`):
  - **The admin plane is remote-only: no admin socket exists inside the container.** Session create/renew/kill/restart are authenticated **remote** operations from the first release, served on the same listener as attach and gated by the admin bootstrap credential (constant-time comparison, failure throttle with backoff, bounded request bodies, a small concurrency cap on in-flight verifications, audited failures). There is no loopback endpoint, no UDS, and no in-container operator CLI.
  - **Why remote-only, and what it eliminates**: locality was never authentication (the shell child may share the container and the UID), so an in-container admin socket would have required proving the caller is not inside a managed session -- which in turn required a kernel-authoritative membership primitive. Removing the socket satisfies that requirement **by construction**: an escaped process has nothing to connect to. It also eliminates the in-container plaintext-credential problem entirely -- with no helper on the presentation path there is no splice-only CLI, no argv/environment/tempfile exposure, and no "dumpable before file-descriptor possession" timing window to satisfy (a constraint that is in any case **unsatisfiable** for a credential-bearing stdin inherited at `exec`, since the descriptor and its buffered bytes exist before the new program can call `prctl`). This is elimination rather than hardening, matching the keyless token model's approach.
  - **Admin-plane-unreachable (MUST), by construction**: no admin operation is reachable from inside a managed session, because no in-container admin endpoint exists. Rationale for the requirement: running `session create/renew` *inside* a managed PTY would print the one-time token into the PTY byte stream, landing it in the scrollback ring and every attach/replay client -- a server-side exfiltration channel no client-storage rule can close. Tokens are returned only to the external control client and are never written to any PTY master. Phase 1 test: the runtime exposes no listening socket, UDS or otherwise, reachable from a session child.
  - **Runtime-side containment that remains**: the runtime is still the mint, so freshly minted tokens and the presented admin credential exist transiently in its memory. It MUST set `PR_SET_DUMPABLE=0` **before any secret enters memory** -- this is what blocks same-UID `/proc/<pid>/mem`, `/proc/<pid>/fd`, and `ptrace` access on Linux, verified by measurement (with dumpable=1 an attacker read the credential out of both `/proc/<pid>/mem` and an inherited pipe via `/proc/<pid>/fd`; with dumpable=0 both were denied). **`prctl` failure is fail-closed**: if `PR_SET_DUMPABLE=0` cannot be set, the runtime refuses to serve before reading any credential input. `PR_SET_DUMPABLE=0` is Linux-specific; non-Linux targets are out of scope for MVP.
  - **Credential handling (MUST)**: the plaintext exists only on the operator's trusted side and reaches the runtime only over the authenticated transport. It is never accepted as an argv flag, never placed in any process environment, never written to temporary files, and never logged -- audit records a fingerprint of the verifier hash, not the credential. Verification is constant-time and the presented buffer is zeroized immediately afterwards; attach-token plaintext is zeroized promptly after hashing.
  - **Phase 1 adversary tests**: `ptrace` and `/proc/<pid>/{mem,fd}` probes against the runtime are **denied (`EPERM` or `EACCES` -- measured, `/proc/<pid>/mem` denial is `EACCES`)**, with a positive control asserting the same probes *succeed* at dumpable=1 so a pass cannot be an artifact of Yama `ptrace_scope`; dumpability is asserted to remain 0 after startup (regression guard -- a dependency calling `prctl(PR_SET_DUMPABLE, 1)` must fail the test); `prctl` failure refuses service; no session-reachable admin socket exists; and teardown is exercised against a `setsid` escapee, a double-fork with an early-exiting intermediate, and a `SIGTERM`-trapping child, asserting the tier's documented semantics (Tier 1: survivors detected and audited; Tier 2: zero survivors).
  - **Issuance at creation**: creating a session mints an immutable `generation` and a fresh attach token, **minted and returned exactly once at creation -- and valid for multiple reattaches** until its expiry or a generation bump (not single-use); reconnecting clients are not locked out, and theft exposure is bounded by a short default TTL (well below the session TTL)
  - **Renewal (`openab-pty session renew <name>`)**: admin-authenticated like create; the session **process survives** (scrollback and state intact), the generation is bumped (all outstanding tokens for the session become invalid immediately), and a fresh attach token is returned exactly once. **Renew-while-attached, defined**: an actively attached connection is terminated via **connection-evict** (see the named sequences in Session lifecycle -- never session-teardown) with a **renew-distinct close code**, so an evicted client can tell renewal from takeover. **Renew is admin-initiated and disruptive by design**: it may cut an active session's connection -- including the admin's own if they renew while attached -- which is the correct behavior for its primary use cases (expired or suspected-stolen tokens). Renew is distinct from **restart-in-place** (which replaces the process). MVP tokens are otherwise valid until expiry or kill; there is no client-side refresh on the attach surface
  - **Attach only verifies, never issues**: `GET /pty/{session}` validates the presented token; there is no minting path on the attach surface
  - **Per-session revocation**: kill/recreate bumps the generation and deletes the stored token hash, immediately invalidating outstanding tokens for that session; runtime restart clears all token state (sessions die with the process anyway, so this is not a loss)
  - **Token format (MVP): no signing key exists.** Each attach token is a CSPRNG 256-bit opaque bearer value; the runtime stores only its hash together with `(session ID, generation, scope = attach-only, expiry)` in memory and deletes it on kill/expiry. Because sessions deliberately do not survive a runtime restart, self-contained signed tokens buy nothing in MVP -- and eliminating the signing key eliminates the minting authority a same-container shell could steal. Signed (HMAC) tokens are a later option and require either an external signer outside the PTY container or the runtime/child privilege boundary below
- **Command authority**: the spawned command is operator configuration only; clients can never specify it. Session names are allowlist-validated (`[a-z0-9-]{1,32}`)
- **Isolation**: the PTY container mounts only the workspace volume (workspace-scoped, never the broker HOME PVC) and its own config projection; no service-account token, no broker config, no platform secrets (see the deployment contract above). NetworkPolicy applies at pod scope: it can restrict the standalone profile's pod independently; in the colocated profile it cannot separate the two containers (see Isolation tiers)
- **Container defaults**: the `openab-pty` image runs as a non-root user (UID 1000), `allowPrivilegeEscalation: false`, capabilities dropped, `readOnlyRootFilesystem` with the workspace as the only writable persistent mount plus the `/run/openab-pty` tmpfs (see Filesystem layout); child UID separation is optional hardening per Same-container containment above
- **Rate limiting in MVP**: per-IP WS-upgrade failure limits (e.g. 5 failures/min then a short ban) ship in Phase 1 -- audit is detection, rate limiting is prevention. **The admin control plane is throttled too, not just WS upgrades**: the remote admin endpoints enforce a failure throttle with backoff, bounded request/body sizes, and a small concurrency cap on in-flight verifications (bounded work per attempt); admin auth failures are audited like attach failures
- **Client-side token storage (contract for the Phase 2 web client)**: the attach token is shell-equivalent, so the shipped client holds it **in memory only** -- never localStorage/sessionStorage, never in URLs (query strings leak via history, referrer, and proxy logs), never in cookies. Page reload = token gone = re-issue via renew, accepted UX. The served page sets a restrictive CSP; these rules are a Phase 2 acceptance criterion, stated now so the client is not designed around persistent storage
- **Audit in MVP**: attach/detach, session create/kill, and auth failures are logged from Phase 1; a leaked token must be observable
- **Env**: the PTY child gets an explicit allowlist (TERM, LANG/LC_*, PATH, HOME, USER, SHELL) and nothing else; `OPENAB_*` and cloud-credential variables are never inherited

### Session lifecycle (owned by `openab-pty`, designed for byte streams)

- **Liveness**: activity = client input OR PTY output OR a live attached socket (WS ping/pong at a 15-30s interval; a half-open socket counts as detached after 2-3 missed pings -- exact values are Phase 1 config with these recommended defaults, balancing flaky mobile networks against dead-client slot pinning)
- **TTLs**: detached-idle TTL (default 30m) plus an absolute session lifetime cap (default 12h) that applies even while attached -- capacity cannot be pinned forever by an open browser tab. Expiry is client-visible: a warning control frame precedes forced teardown, and the WebSocket closes with a distinct close code so clients surface "session expired" instead of retrying a network error
- **Attach semantics (MVP)** -- single-attach exclusive; multi-viewer is Phase 3:
  - **Exclusivity mechanism**: a session-level `owner_conn_generation` compare-and-swap -- only the connection that wins the CAS holds the PTY write end; the replaced connection's write path is dropped before its socket closes; the PTY writer task honors only the current generation; teardown of a replaced connection can never affect its successor
  - **Takeover**: a second attach with a valid token takes over via this CAS (documented behavior)
  - **Takeover abuse controls**: every successful preempt is audited as an anomaly event (session, source address, count), and preempt frequency is rate-limited per session (e.g. max N takeovers/min, then attaches are rejected with a distinct close code) -- a stolen still-valid token must not be able to silently ping-pong the CAS and starve the legitimate client
  - **Limiter scope (generation-fenced)**: a generation bump (renew/recreate) resets the bucket, and the first attach under a new generation always bypasses an exhausted bucket -- so a thief who exhausted the budget can never lock the victim out of the `session renew` recovery path
  - **Lock ordering (total)**: token revocation (generation bump + stored-hash deletion), the attach CAS, and replay registration execute in that order under the session lock; where the buffer lock is also needed (replay registration), the session lock is always acquired before the buffer lock and never the reverse -- no interleaving where a revoked token wins an attach, a replay registers against a stale generation, or two paths deadlock across the two locks
  - **Locks cover state, never I/O**: the session lock protects the state machine (generation, token hashes, `owner_conn_generation`, subscriber registration metadata); all socket and PTY I/O -- including notifying/closing a preempted connection and draining replay bytes -- executes after the lock is dropped, fenced by generation so stale work is ignored
  - **Teardown is never client-blockable**: kill and TTL paths are never blocked by a per-connection drain (bounded wait or lock-free signal), so a slow or malicious non-reading client cannot delay renew, expiry, or teardown
- **Reconnect**: monotonic byte cursor from day one -- the ring buffer tracks total bytes written; clients reconnect with `since=<offset>` and receive only missed bytes. The replay-to-live handoff is **atomic**: the subscriber registers under the buffer lock, captures the end offset, replays through it, then drains queued live bytes -- with connection-generation fencing so teardown of a replaced connection cannot affect its successor. On overflow the server sends an explicit `gap` control frame (bytes-dropped count) so the client can trigger a full clear/redraw instead of rendering a sliced ANSI stream
- **Output-path bounds (MUST)**: every buffer on the PTY-to-client path is bounded, not just retained scrollback -- the replay-to-live handoff queue and the per-connection outbound backlog each have a fixed cap. A client too slow to drain its backlog gets a `gap` frame (drop-oldest, cursor advances) or, past a hard watermark, is disconnected with a distinct close code -- mirroring the input-side fail-closed backpressure so neither direction can grow unbounded memory
- **Memory admission formula (MUST)**: the per-session bounds compose into an explicit container budget -- `max_sessions × (scrollback_kib + replay/backlog queue caps + fixed per-session overhead) + runtime baseline ≤ the container memory request`. This matters more here than in a stateful service: the keyless in-memory model makes an OOM a **total-loss event** (every session and token invalidated at once, per Consequences), so admission control is load-bearing, not tuning. The chart derives a recommended memory request from the configured `[pty]` values and refuses obviously-oversubscribed combinations; Phase 2 ships the observability for it (per-session buffer occupancy, global tracked-process and FD counts, admission-rejection metrics) plus capacity guidance
- **Disk admission (MUST)** -- the same reasoning applies to storage, and the failure is equally shared: the workspace volume is common to every session and, on ECS Fargate, the task's ephemeral allocation **also holds the container images**. A single session running a large build, cloning a big repository, or spewing logs can therefore exhaust the volume and make **the co-resident broker's writes fail too**. Requirements: a configured per-session disk budget with a documented total (`max_sessions × per_session_disk + image and runtime overhead ≤ the provisioned volume`), a usage metric per session, and **"workspace volume full" surfaced as a first-class client-visible condition with its own control frame** rather than an opaque write error inside the terminal. Quotas are advisory where the substrate cannot enforce them (Fargate provides no per-container quota for bind mounts) -- in that case the budget is monitoring plus a documented operator limit, and that limitation is stated rather than implied
- **`scrollback_replay` vs cursor semantics** (distinct controls): incremental `since` replay is always available within the ring buffer's retention; `scrollback_replay` governs only the cursor-less full-history dump on a fresh attach (default off -- secrets-safe); setting `scrollback_kib = 0` disables retention entirely, which also disables `since` replay (every reconnect starts with a `gap` + reset)
- **Two named termination sequences** (they are different operations and must never be conflated):
  - **Connection-evict** -- ends a *connection*, the session process survives: notify the client, close the socket with the operation-specific close code (takeover, renew, TTL warning). Used by attach takeover and renew-while-attached
  - **Session-teardown** -- ends the *session*: setpgid on spawn; SIGTERM-grace-SIGKILL escalation; evict-while-attached order = notify client, close socket, kill (per the Kill domain below), close master fd, release slot; buffers cleared on teardown; scrollback never touches disk
- **Kill domain -- two tiers, detected at startup.** The process group is only the first signal path, not a containment guarantee (a child that calls `setsid` or double-forks escapes the pgid). Two distinct problems are named separately and solved separately: **attribution** (which session does a live process belong to?) and **reaping** (collecting exited children). **Measured facts this design rests on** (feasibility spike, kernel 6.8, non-root/`drop: ALL` container):
  - Ancestry cannot attribute. `PR_SET_CHILD_SUBREAPER` guarantees reaping, not lineage: with two concurrent sessions each leaking a double-fork orphan whose intermediate exits before any scan, **both orphans reparent to the runtime and neither is attributable to its session**, while the reaper collects both intermediates successfully. Reaping succeeds exactly where attribution fails.
  - `pgid`-only teardown leaks (a `setsid` escapee survived); `SIGTERM`-only teardown leaks (a trapping child survived); `cgroup.kill` converged in ~10 ms against all four adversaries with zero survivors, including 51 live processes forking during teardown -- the kernel closes that race, so no userspace kill-and-rescan loop is needed.
  - A writable cgroup2 mount is **not** available under the default container contract (`mkdir` → `EROFS`, denied at the mount, ownership irrelevant), and establishing delegation requires a privileged actor **after** container start -- an initContainer cannot do it, because the container's own cgroup does not exist while init runs.

  Because the runtime's pods and tasks are disposable by design (see Consequences), MVP does not buy the hard guarantee at the price of a privileged component:

  - **Tier 1 -- default, no prerequisites: best-effort teardown.** `setpgid` on spawn, `SIGTERM`-grace-`SIGKILL` escalation over the process group, plus `PR_SET_CHILD_SUBREAPER` and a `pidfd` per spawn-tracked child for reaping, self-exit detection, and defense-in-depth signalling. **Teardown convergence and the absolute TTL are best-effort in this tier and MUST be labelled as such** -- a process that escapes the pgid may survive until the pod or task is replaced, which is the architecture's normal reclamation path. Survivors are audited as anomalies and counted on a leak metric so the condition is observable rather than silent.
  - **Tier 2 -- optional hardening, when delegation is arranged: one cgroup per session, torn down via `cgroup.kill`.** Membership is read from the kernel (`cgroup.procs`), never inferred from ancestry; teardown is `cgroup.kill` then wait for empty; convergence and the TTL become hard guarantees. Requires cgroup v2 with subtree delegation reaching the container's **own** cgroup — only a descendant of it works (an unrelated subtree fails `EACCES` on delegation containment; a pod-level sibling fails `ENOENT` on `nsdelegate`). No *controller* delegation is needed: `cgroup.procs`, `cgroup.events`, and `cgroup.kill` are core cgroup v2 files. Non-Kubernetes systemd deployments get this shape natively via `Delegate=yes`.
  - **Startup detection, not fail-closed on absence**: the runtime probes for Tier 2 end-to-end (create → populate → `cgroup.kill` → remove a probe cgroup) and selects the tier accordingly, logging which one is active and, when Tier 2 is unavailable, the distinguishing reason: `EROFS` (no writable cgroup mount), `EACCES` (subtree not delegated), `ENOENT` (outside the cgroup namespace), or a blocked syscall. Seccomp profiles commonly filter `prctl`/`pidfd_open`; the probe is the intended detection for all such environments. **Operators who require the hard guarantee configure Tier 2 explicitly, and in that mode absence of delegation IS fail-closed** -- the runtime never silently downgrades a guarantee an operator asked for.
  - **Spawn mechanics**: `fork` + `setpgid(0,0)`, then (Tier 2) write the child's own pid to a pre-opened `cgroup.procs` fd, **before `execv`** -- so adversary code never runs outside its cgroup, and the pre-exec step stays async-signal-safe (no allocation). **`clone3(CLONE_INTO_CGROUP)` MUST NOT be the default**: measured `ENOSYS` under seccomp `RuntimeDefault` while succeeding as host root and under `seccomp: Unconfined`, i.e. filtered rather than unsupported. Detect it and prefer it opportunistically. `PR_SET_CHILD_SUBREAPER` and `pidfd_open` are *not* filtered by `RuntimeDefault`.
  - **Resource budget**: tracked processes and their pidfds are capped per session and globally, with reserved FD headroom for control/WebSocket sockets; hitting tracking capacity is fail-closed (the session is killed, never left partially tracked)
  - **Membership queries (Tier 2)** scan `<session>/cgroup.procs` through the directory handle the runtime itself created. **Do not parse `/proc/<pid>/cgroup`**: it returns a cgroup-namespace-relative path (`/oab-sessions/sess-x`), not the mount-relative path the runtime used, so string matching is fragile.
- **Self-exit, defined (Phase 1 behavior, not deferred with the full state machine)**: when the child exits on its own, the runtime reaps it (kill-domain convergence still runs for surviving descendants), sends any attached client a final output flush plus a **session-ended close code** (distinct from TTL expiry and eviction), releases the slot after convergence, and deletes the session's token state -- the name then behaves exactly like reattach-to-dead: a distinct error offering restart-in-place. Termination classes (user-kill / self-exit / runtime-shutdown) are tagged in audit from Phase 1; the richer state machine remains Phase 3
- **Recovery taxonomy** (stated, not implied): detach/reattach survives (process alive); pod restart does not (process dead) -- reattach-to-dead returns a distinct error and offers **restart-in-place**: same session name, a fresh process and a new generation (old tokens invalid, empty scrollback). Pod-lifetime durability is out of scope and documented as such
- **Ephemerality is explicit in the product surface (MUST)** -- a terminal *looks like* a local shell, so users will assume files persist. Statelessness is deliberate here (it is the same stance the broker takes, with the same `pre_boot`/`pre_shutdown` externalisation points), which is exactly why it must be surfaced rather than discovered:
  - an **attach-time notice** stating that the workspace is ephemeral and naming how to externalise work
  - a **distinct close code for "runtime/task replaced"**, separate from TTL expiry, takeover, and self-exit, so clients can report "your task was replaced, the workspace was reset" instead of surfacing a generic network error
  - documentation naming `git push` (or an equivalent push to durable storage) as the **primary** externalisation path and lifecycle hooks as backup -- the rolling-deploy ordering means a replacement task can start before the departing one's `pre_shutdown` has finished writing, which is acceptable for bot state and less so for user-authored work
  - `openab-pty` **reuses the broker's existing `pre_boot`/`pre_shutdown` hooks** rather than inventing a mechanism: same contract, same documentation, no new concepts
  - durable workspace storage (an RWX volume, or EFS on ECS) is an **optional operator upgrade**, never a requirement

---

## 3. Consequences

### Positive

- OAB keeps its thin-broker identity untouched — zero changes to the shipped binary, pool, or ACP path
- Fills the remote + sandboxed + raw-terminal quadrant with a real container boundary instead of a claimed one
- Highest reversibility: default-off, separately versioned, separately deprecable
- Coexistence where it matters (shared workspace) without shared process or credential-mount domains; in MVP coexistence is two separate pods on any storage class (mechanism A or B), so the security story is single-tier: full isolation only
- The Phase 4 notification bridge (broker pulls from `openab-pty` -> relays to Discord) later reconnects the feature to OAB's messaging strength without merging the runtimes -- demand-gated together with the colocated profile

### Negative

- A second binary and image to build, test, and release (mitigated by the existing multi-binary workspace and release pipeline)
- **Adjacency on block storage costs a scheduling constraint**: mechanism B pins both pods to one node, reducing scheduling freedom and sharing node fate (though not pod fate). Deployments wanting free scheduling need a shared-filesystem class
- **Teardown is best-effort by default**: in Tier 1 a process that escapes the process group can outlive its session until the pod or task is replaced. Capacity accounting and the absolute TTL are therefore approximate, and the ADR labels them so. Operators needing hard guarantees arrange cgroup delegation for Tier 2, which requires a privileged actor outside this container -- deliberately not an MVP requirement
- **The keyless in-memory model's cost, consolidated**: every session and token is bound to one runtime process -- no HA, no multi-replica serving, no failover; a crash, OOM, restart, projection rollout, or admin-credential rotation (which requires a restart) invalidates **all** sessions and tokens simultaneously. This is the deliberate exchange for eliminating at-rest minting authority, and it is why the rotation runbook (Phase 2) must state the blast radius up front
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

Leaves the need unserved; users accept Herdr's laptop fragility or OpenDray's host blast radius. The composable-runtime form lets OAB serve it without betting the broker's identity.

---

## 5. Implementation Plan

**Pre-implementation gate (go/no-go)**: Phase 1 starts only after the demand component passes. The feasibility component is **complete**:

- **Demand component (numbers stated, still open)**: at least **3 independent user requests** (distinct users; issues, discussions, or support threads — the originating discussion thread and its participants count as one) within **90 days** of this ADR merging, plus a maintainer-agreed operating-cost budget recorded on the tracking issue. These defaults may be revised by a maintainer on the tracking issue *before* the gate is evaluated, never retroactively.
- **Feasibility component — DONE.** A time-boxed spike in a non-root, capabilities-dropped container measured the kill-domain and credential-containment mechanisms; its findings are what produced the two-tier kill domain, the remote-only admin plane, the `clone3` correction, and the substrate matrix in this ADR. Headline results: ancestry provably cannot attribute sessions (two concurrent double-fork orphans, neither recoverable, while reaping succeeded); `cgroup.kill` converges in ~10 ms against four adversaries including a 51-process fork race; a writable cgroup mount is unavailable under the default container contract and cannot be arranged by an initContainer; `PR_SET_DUMPABLE=0` is an effective same-UID barrier with the attack demonstrated working when it is off. Evidence is recorded on the tracking issue.

Acceptance of this ADR records the *design*, not a commitment to build on a schedule; the 12-month adoption review below is the post-ship counterpart of this gate.

### Phase 1: `openab-pty` MVP (new crate, new binary)

- Own session manager: named sessions, operator-configured command, allowlist-validated names
- **Session bootstrap -- remote admin from day one**: sessions are created through the authenticated **remote** admin API (`create` / `renew` / `kill` / `restart`, the last performing restart-in-place for reattach-to-dead per the recovery taxonomy), which spawns the PTY and returns the attach token to the external control client; `GET /pty/{session}` is attach-only. There is no in-container admin socket or operator CLI (see the admin plane in the Security model). Operators reach the endpoint via their Ingress or a port-forward; a thin client CLI runs **outside** the container.
- portable-pty spawner with `setpgid`, escalating kill, the teardown order above, **and the two-tier kill domain** (Tier 1 pgid + subreaper/pidfd best-effort by default; Tier 2 per-session cgroup with `cgroup.kill` when delegation is detected; pre-`execv` pid write, never `clone3(CLONE_INTO_CGROUP)` as the default; startup tier detection with the distinguishing reason logged)
- `GET /pty/{session}` WSS endpoint: binary frames = PTY bytes; text frames = versioned control schema (`resize`, `ping`, `detach`, `gap`, `ttl-warning`) with a defined close-code table. Frame validation is strict allowlist: bounded max frame size, unknown control types rejected, resize values bounds-checked; malformed frames count toward an abuse metric and can disconnect
- Input backpressure: per-connection write watermark toward the PTY master; a client exceeding it is disconnected (fail closed) rather than growing unbounded queues or stalling the reader
- Auth: the token control plane above (authenticated create, one-time issuance bound to session generation, attach-only verification); fail-closed off-loopback; `/acp`-style browser subprotocol transport; per-IP upgrade-failure rate limiting
- Monotonic cursor reconnect with atomic replay/live handoff and gap signaling; scrollback in-memory, off-by-default fresh-attach replay, cleared on teardown
- Detached-idle TTL + absolute lifetime cap (with client-visible expiry warning + close code); single-attach exclusive
- Audit log (attach/detach/create/kill/auth-failure) and basic metrics
- `openab-pty --validate-projection <file>`: the fail-closed startup guard exposed as a standalone subcommand, so operators can verify a hand-generated projection from day one (Phase 2 CI reuses it as the guard test)
- **Extract the security/lifecycle MUST inventories into `docs/pty-security-contract.md`** (given/when/assert form, one clause per MUST) so the adversary tests below are CI-linkable to named contract clauses; the ADR's MUST sections then become pointers into that contract (see the Granularity note in the Security model)
- **Adversary tests** (see the Security model for the full list): `ptrace` and `/proc/<pid>/{mem,fd}` probes against the runtime are **denied (`EPERM` or `EACCES`)** with a **positive control** proving the same probes succeed at dumpable=1, so a pass cannot be an artifact of Yama `ptrace_scope`; a dumpable regression guard; `prctl` failure refuses service; no session-reachable admin socket exists; and a teardown suite (`setsid` escapee, double-fork with early-exiting intermediate, `SIGTERM`-trapping child) asserting the active tier's documented semantics -- Tier 1 detects and audits survivors, Tier 2 converges to zero
- Resize propagation (TIOCSWINSZ) including attach-time initial size
- Terminal-capability response filtering at the PTY boundary (known Ink-CLI startup breakage)

### Phase 2: Deployment + web client

- Helm: independent `openab.enabled` / `pty.enabled` toggles rendering **separate pods** (`--set profile=acp|pty`); standalone profile gets its own Service/Ingress (`/pty/*`) and NetworkPolicy example; config split documented per the configUrl pattern; `ghcr.io/openabdev/openab-pty` image published from the existing release pipeline. **The MVP chart does not render the colocated `full` profile at all** -- profile 3 ships only if its demand gate opens (see Later), and when it does it is opt-in only, never the default, with its values file labeled convenience-only and pointing at the Isolation tiers table. Both adjacency mechanisms ship as values examples: A (RWX, no affinity) and B (RWO with pod affinity pinning both pods to one node), plus the `ReadWriteOncePod` and UID-alignment warnings above
- **Web client is attach-only (browser management deferred)**: the minimal xterm.js page served by `openab-pty` accepts an attach token and connects -- nothing else. Remote list/create/kill/renew endpoints exist for *non-browser* admin tooling only, gated by the admin bootstrap credential; **the web client never receives, stores, or transmits the admin credential** -- delivering the global management credential to a browser would turn one XSS into administration of every session. A browser management UI requires an operator-mediated pairing / one-time scoped-issuance flow or the identity layer, and is explicitly deferred until one exists
- **Client-page acceptance criteria (testable)**: attach token held in memory only (never localStorage/sessionStorage/cookies/URLs); page served with CSP enforcing at minimum `script-src 'self'` (no inline/eval), `object-src 'none'`, `base-uri 'none'`, `frame-ancestors 'none'`, and `connect-src` limited to the PTY origin; no third-party runtime scripts
- **Admin-credential rotation runbook (acceptance criterion)**: documented steps, blast radius, and expected downtime for rotation (generate new value -> update delivered hash -> restart runtime -> all sessions cleared) -- operators must not discover mid-incident that rotation kills every session
- Rollback contract (Phase 2 acceptance criterion): disabling the toggle drains (notify + grace, honoring `terminationGracePeriodSeconds`) then kills sessions; the broker container is unaffected and not restarted. Projection updates roll the PTY container only -- live sessions die on the rollout (non-persistent by contract) and the chart documents this; the workspace PVC is untouched by disable/re-enable, and re-enable is a fresh runtime with zero sessions
- Projection tooling has an owner (Phase 2 acceptance criterion): the Helm chart generates both config views, and CI runs a **guard test** -- the delivered PTY projection is fed to `openab-pty --validate-projection` (the Phase 1 startup validator exposed as a subcommand), which must accept it and must reject a deliberately poisoned projection (embedded `[secrets.refs]`, `${secrets.*}`, or a broker section). The "never delivered" invariant is thereby enforced by mechanism, not operator discipline

### Phase 3: Lifecycle hardening

- Multi-viewer (one writer, N readers) with writer-lease semantics and read-only token scope
- Reconnect backoff, richer capacity controls (per-token limits)

### Phase 4: Messaging bridge (demand-gated with profile 3; colocated profile only)

> This phase exists only if the profile-3 demand gate opens (see Later): the bridge is colocated-only by design, so it inherits profile 3's gate. Recorded here as the target design.

- `openab-pty` exposes a pod-local, loopback-only notification stream; the **broker pulls** (long-poll/SSE on localhost) when a detached session emits no output for N seconds after a prompt-like burst (stated heuristic, not magic); the broker relays to the platform thread. Bridge is one-way and feature-gated
- **No bridge secret enters the PTY container -- by design, stated now**: a delivered HMAC key would recreate exactly the in-container authority the keyless token model eliminated (a same-UID child can read any file the runtime can read; 0400 at the same UID is not a boundary). The pull model removes the broker-side ingress entirely: there is no webhook endpoint to leave open and no shared key to steal. Residual risk, stated: a same-UID child that kills the runtime (an accepted same-UID risk) could bind the freed port and forge events -- therefore the broker treats bridge events as **display-only, rate-limited hints**: they never carry commands, never mutate broker state, and are labeled best-effort in the relayed message. A push/webhook variant with an HMAC secret is permitted only with an external signer outside the PTY container or the runtime/child privilege boundary from the Security model (non-MVP hardening)
- **Not available in the PTY-only profile** — there is no broker to relay through, and `openab-pty` will not grow its own notifier (that would recreate the scope creep this ADR exists to avoid). Users who want notifications deploy profile 3
- **Pull-stream resource contract**: the notification stream is bounded like every other surface -- at most **one** concurrent broker stream with **incumbent-wins admission**: while a healthy stream exists (heartbeating within its idle timeout), new connection attempts are rejected; a replacement is admitted only after heartbeat timeout declares the incumbent dead. This closes both the churn vector (forced close/reconnect work) and the live-hijack vector -- **stated residual**: the stream is unauthenticated pod-local, so a same-pod process that connects *first* (or after killing the runtime and binding the freed port) can occupy it; display-only rate-limited hints bound the impact in every case. Heartbeat with an idle timeout on both ends, reconnect with capped exponential backoff on the broker side, and a fixed-size event queue in `openab-pty` with coalescing (per-session dedupe: a newer idle event replaces an older undelivered one) and drop-oldest overflow. "Rate-limited hints" thus constrains retained resources, not only delivery semantics

### Later (demand-gated, explicitly deferred)

- **Colocated profile 3 + the Phase 4 bridge**: gated on the standalone profile proving demand *and* an explicit maintainer decision to re-open; the sidecar design, partial-isolation tier, and pull-bridge contract recorded in this ADR are the target design for that gate. Until then the chart renders separate pods only
- Shared lifecycle crate extraction (if the ACP pool and PTY manager converge naturally). Candidate shared surface: spawn mechanics, env-allowlist construction, pgid kill/escalation; deliberately NOT shared: liveness definitions, TTL/eviction policy, persistence
- **Adoption review point**: 12 months after the standalone profile ships, review its usage; below a threshold the maintainers set then, consider deprecating the standalone image or folding PTY back to colocate-only
- Single-process merge (only if operations prove the runtime split is more cost than benefit)
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
| Multiple read projections | `visible` / `recent` / `recent-unwrapped` / `detection` views of one PTY | Later (demand-gated) -- no phase deliverable exists; if adopted, views are lazy, bounded, and charged to the owning session's memory envelope |
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

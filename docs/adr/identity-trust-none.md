# ADR: Identity Trust-None Default & Trust Pyramid

- **Status:** Proposed (v2 — revised per PR #1263 review feedback)
- **Date:** 2026-06-30 (revised 2026-07-04)
- **Author:** @chaodu-agent
- **Reviewers:** @pahud, @howie
- **Tracking issues:** #1262
- **Depends on:** [First-Class Per-Platform Configuration](first-class-platform-config.md) — per-platform `allowed_users` live in the first-class `[platform]` sections defined there.

---

## 1. Context & Decision

Flip the default trust model from **allow-all** to **identity trust-none**: when a
platform's `allowed_users` is empty and `allow_all_users` is not explicitly set to
`true`, deny all incoming messages and echo the sender their own ID so they can
request access.

Trust is enforced at a **dedicated ingress layer** — the Trust Gate — that sits
between the platform Receiver and the per-platform Handler. This is a structural
guarantee: no event reaches any Handler (or the Dispatcher / Agent) without passing
through the gate. The gate is **not** inside any adapter — it is an independent
layer that all adapters are wired through.

## 2. Motivation: trust-all default is insecure

All adapters currently auto-detect: empty `allowed_users` → `allow_all_users = true`.
A fresh deployment trusts **everyone** by default. For publicly discoverable bots
(e.g. anyone can DM a Telegram bot), this means any stranger can drive the agent.

Additionally, trust checks are currently **scattered** across adapters — each one
implements its own variant (`is_denied_user()` in Discord, `should_skip_event()` in
Gateway, inline allowlist in Slack). This means:
- Different implementations doing the same thing
- A new adapter forgetting the check = fully open bot
- No architectural guarantee that trust is enforced

## 3. Trust Pyramid (Defense in Depth)

Three layers with **clearly separated responsibilities** — only L1 and L3 are
security boundaries. L2 is operator scoping, not authorization.

```
                          ▲
                         ╱ ╲
                        ╱   ╲
                       ╱ L3  ╲         🔒 Layer 3: Identity Trust Control  (SECURITY)
                      ╱       ╲        allowed_users per platform — default DENY-ALL
                     ╱ sender  ╲       "Is THIS IDENTITY allowed?"  covers every path incl. DMs
                    ╱  allowed? ╲
                   ╱─────────────╲
                  ╱               ╲
                 ╱      L2         ╲    🔓 Layer 2: Channel/Group Scope Control  (NOT security)
                ╱                   ╲   allowed_channels, allowed_groups, allow_dm — default OPEN
               ╱  surface open?      ╲  "Which CONVERSATION SURFACES does the bot engage in?"
              ╱  (channel/group/DM)   ╲  optional operator scoping (noise/cost), not authorization
             ╱─────────────────────────╲
            ╱                           ╲
           ╱           L1                ╲   🔒 Layer 1: Platform Authentication  (SECURITY)
          ╱                               ╲  "Is this request REALLY from the platform?"
         ╱   webhook signature / JWT /     ╲
        ╱    secret token / IP range        ╲
       ╱─────────────────────────────────────╲
```

**Default posture:** L1 always on (edge) · **L2 open** unless explicitly disabled · **L3 deny-all** unless explicitly allowed.

### Layer 1: Platform Authentication (gateway layer — transport)

Verifies the request is genuinely from the platform, not spoofed. The **only**
security check at the gateway level.

| Platform | Auth Mechanism | How it works |
|----------|---------------|--------------|
| **Telegram** | Secret Token + IP Range | `X-Telegram-Bot-Api-Secret-Token` header; source IP in Telegram subnet (149.154.160.0/20, 91.108.4.0/22) |
| **LINE** | HMAC-SHA256 Signature | `X-Line-Signature` = HMAC(channel_secret, request_body) |
| **Feishu** | SHA256 Signature + Encrypt Key | SHA256(timestamp + nonce + encrypt_key + body) |
| **WeCom** | Token Signature + AES Decrypt | SHA1(sort(token, timestamp, nonce, encrypt)); AES-256-CBC body decryption |
| **Google Chat** | JWT (RS256) | Bearer token verified via Google JWKS; email claim = `chat@system.gserviceaccount.com` |
| **MS Teams** | JWT (OpenID Connect) | RS256 JWT verified via Bot Framework OpenID metadata + JWKS |
| **Slack** | Socket Mode WebSocket | App-Level Token (xapp-...) authenticates WS connection |
| **Discord** | Gateway WebSocket | Bot Token authenticates WS connection |

### Layer 2: Channel/Group Scope Control (core layer) — NOT a security boundary

Controls **which conversation surfaces** the bot engages in — channels, groups,
and DMs (`allow_dm`). Already implemented.

This is **operator scoping, not authorization**. The platform itself already
guarantees the bot only receives events from channels/groups it is a member of
with read permission — you cannot receive a message from a channel you were never
added to. So `allowed_channels` does not defend against "unauthorized channels"
(L1/the platform already does); it only narrows an over-permissioned bot to the
surfaces an operator wants it active in. Its value is noise/cost control.

**Default: OPEN** (`allow_all_channels = true`, `allow_dm = true`). Operators
*disable* surfaces only for hard scoping (e.g. a group-only bot sets
`allow_dm = false`).

**DMs are an L2 surface with a critical asymmetry:** unlike groups, a DM has **no
platform membership gate** — anyone can open a DM with a public bot. So when
`allow_dm = true`, the **only** protection on that path is L3. Enabling the DM
surface is an L2 decision; guarding who may use it is L3.

### Layer 3: Identity Trust Control (core layer) ← This ADR — the SECURITY gate

Controls which individual senders can trigger agent actions. Currently defaults
to allow-all; this ADR flips it to **deny-all**. This is the one authorization
boundary at the policy layer, and it covers **every** ingress path — including
DMs, where it is the sole protection.

**Why L2 must stay open for the deny UX to work:** the "echo your UID so you can
request access" reply only fires if an untrusted sender's message actually
*reaches* L3. If L2 defaulted closed (e.g. `allow_dm = false`), a new user would
be silently dropped at the scope layer with no path to onboard. L2-open + L3-deny
gives the intended self-service flow:

```
stranger messages the bot
  → L1 ✅ authentic platform request
  → L2 ✅ surface open by default (channel / DM)
  → L3 ❌ identity not in allowed_users
  → echo "⚠️ You're not trusted. Your ID: 123456789. Ask the admin to add you."
  → drop — no agent action
```

This flips **only L3** from today's allow-all to deny-all; L2 stays open. Minimal
breaking surface, maximal safety: nothing acts for an untrusted identity, yet
strangers still get a way to request access.

## 4. Decision

### 4.1 Trust-none default (identity layer)

```
Current:  empty allowed_users → allow_all_users = true  (TRUST ALL)
Proposed: empty allowed_users → allow_all_users = false (TRUST NONE)
```

When a message arrives from an untrusted sender:
1. Log the event (sender ID, platform, timestamp)
2. Reply with an echo message showing the sender their own ID
3. Do NOT dispatch to any agent

**Semantics of `allowed_users`:**
- Missing key = empty list = deny-all (unless `allow_all_users = true`)
- Empty string sender_id = always denied (fail-closed, regardless of `allow_all_users`)
- Startup validation: warn when a platform section has neither `allowed_users` nor
  explicit `allow_all_users = true` — helps operators catch misconfiguration.

### 4.2 Three-layer adapter architecture (Receiver → Trust Gate → Handler)

Trust enforcement happens in a **dedicated ingress layer** — the Trust Gate —
that is structurally between the Receiver and the Handler. This is NOT inside
any adapter. It is an independent layer that every platform flows through.

**Architecture: Receiver → Trust Gate → Handler**

Each adapter is split into two components with the Trust Gate in between:

| Layer | Responsibility | Per-platform? |
|-------|---------------|---------------|
| **Receiver** | Connect, listen, L1 verify, normalize to `InboundEvent` | Yes |
| **Trust Gate** | L2 scope check + L3 identity check (`decide()`) | **No — unified** |
| **Handler** | Platform-specific interaction logic + dispatch | Yes |

**Why this order:**
- Trust Gate is upstream of Handler — Handler never sees untrusted events
- Slash commands (`/reset`, `/cancel`) are in the Handler — they are gated
- No adapter can bypass the gate — it is architecturally mandatory
- New platform = write Receiver + Handler; trust is automatic

**Type-level enforcement (compile-time hardening):**
The private constructor makes *accidental* bypass a compile error. The Trust Gate
consumes `InboundEvent` and produces a **different type** — `GatedEvent` — which
is the only type Handler accepts. No code outside `crate::trust::gate` can
construct `GatedEvent`, ensuring untrusted events cannot reach Handlers through
normal code paths:

```rust
// In crate::trust::gate (narrow module — only Trust Gate code lives here)
// Note: InboundEvent is defined in crate::trust (mod.rs) and re-exported publicly.
// GatedEvent and seal() live here in gate.rs — keeping the constructor private.

/// Receiver produces this (untrusted). Defined in crate::trust::mod.rs (public).
pub struct InboundEvent { /* ... */ }

/// Trust Gate produces this (trusted).
/// The inner field is PRIVATE (not pub(crate)) — only code in this module
/// can construct it. This makes accidental bypass a compile error.
pub struct GatedEvent {
    inner: InboundEvent,  // private — only gate module can construct
}

impl GatedEvent {
    /// Read-only access to the trusted event data.
    pub fn event(&self) -> &InboundEvent { &self.inner }
    pub fn platform(&self) -> &str { &self.inner.platform }
    pub fn sender_id(&self) -> &str { &self.inner.sender_id }
    pub fn channel_id(&self) -> &str { &self.inner.channel_id }
    pub fn is_dm(&self) -> bool { self.inner.is_dm }

    /// Consuming unwrap — restricted to `pub(crate)` to minimize escape paths.
    /// Only used by the Dispatcher when it needs ownership of the inner event
    /// for session creation. Handlers should use read-only accessors above.
    ///
    /// TRUST BOUNDARY NOTE: The trust enforcement boundary is at the MODULE level
    /// (only crate::trust::gate can CONSTRUCT GatedEvent), not the crate level.
    /// Any code within openab-core can CONSUME a GatedEvent via into_inner(),
    /// but cannot forge one. If the crate grows large or is split, consider
    /// tightening this to a trait-based accessor or moving Dispatcher session
    /// creation into the trust module. Phase 1 SHOULD add a #[doc(hidden)]
    /// annotation or clippy restriction lint to limit call sites.
    pub(crate) fn into_inner(self) -> InboundEvent { self.inner }

    /// Test-only constructor — allows Handler unit tests to create GatedEvent
    /// without wiring the full Trust Gate pipeline. Excluded from production
    /// binaries via cfg(test).
    #[cfg(test)]
    pub(crate) fn assume_trusted_for_test(event: InboundEvent) -> Self {
        Self { inner: event }
    }
}

/// Only constructible within this module (the Trust Gate).
/// No other module in the crate can call this.
fn seal(event: InboundEvent) -> GatedEvent {
    GatedEvent { inner: event }
}

/// Handler signature — cannot accept InboundEvent directly.
/// Cannot forge GatedEvent (private field, no public constructor).
async fn handle(&self, event: GatedEvent) { /* ... */ }
```

**Module layout for enforcement:**
```
crate::trust
├── mod.rs          // PlatformTrustConfigs, TrustConfig, Decision, InboundEvent (public)
├── gate.rs         // gate_event() + GatedEvent (constructor is private)
└── (no other module can construct GatedEvent)
```

A Handler that tries to accept `InboundEvent` directly will not compile.
Any module outside `crate::trust` that tries to construct `GatedEvent` will fail
(private field, no public constructor). This makes accidental bypass a compile
error — the type system enforces the trust boundary structurally. The consuming
unwrap (`into_inner`) is intentionally `pub(crate)` to minimize escape paths
while allowing the Dispatcher to take ownership when creating sessions.

**Trust lookup key:** The gate uses the **per-event platform** from
`InboundEvent.platform` (which maps to `ChannelRef.platform`), NOT
`adapter.platform()`. This correctly handles unified mode where a single
`UnifiedGatewayAdapter` (whose `platform()` returns `"unified"`) multiplexes
events from multiple real platforms.

## 5. Architecture

```
┌──────────────────────────────────────────────────────────────────────────┐
│                         Platform Sources                                   │
├──────────────┬───────────────┬────────────────┬─────────────────────────┤
│   Discord    │    Slack      │    Gateway     │         Cron            │
│  (WebSocket) │ (Socket Mode) │  (TG/LINE/..) │      (timer)            │
└──────┬───────┴───────┬───────┴───────┬────────┴────────────┬────────────┘
       │               │               │                     │
       ▼               ▼               ▼                     ▼
┌──────────────────────────────────────────────────────────────────────────┐
│                Receivers (per-platform transport)                          │
│                                                                          │
│  ┌──────────┐  ┌──────────┐  ┌──────────────┐  ┌──────────┐            │
│  │ Discord  │  │  Slack   │  │   Gateway    │  │   Cron   │            │
│  │ Receiver │  │ Receiver │  │  Receiver    │  │ Receiver │            │
│  └──────────┘  └──────────┘  └──────────────┘  └──────────┘            │
│                                                                          │
│  Responsibilities:                                                       │
│  • Connect & listen (WebSocket / HTTP webhook / timer)                   │
│  • L1 authentication (verify signature / JWT / token)                    │
│  • Normalize → InboundEvent { platform, sender_id, channel_id, is_dm }  │
│                                                                          │
│  Does NOT:                                                               │
│  • ❌ Check allowed_users                                                │
│  • ❌ Handle slash commands                                              │
│  • ❌ Evaluate @mention / multibot / role logic                          │
│                                                                          │
│  EXCEPTION — LINE @mention pre-filter:                                   │
│  • LINE Receiver drops non-@mention group messages BEFORE producing      │
│    InboundEvent (prevents ordinary chatter from hitting Trust Gate        │
│    and triggering deny-echo spam). This is a deliberate exception to     │
│    "Receiver = pure transport."                                          │
└──────────────────────────────────┬───────────────────────────────────────┘
                                   │
                                   │  InboundEvent (unified format)
                                   ▼
┌──────────────────────────────────────────────────────────────────────────┐
│           🔒  TRUST GATE (L2 scope + L3 identity, unified)  🔒            │
│                                                                          │
│  PlatformTrustConfigs::decide(                                           │
│      event.platform,      // per-event platform (not adapter.platform()) │
│      event.channel_id,                                                   │
│      event.is_dm,                                                        │
│      event.sender_id,                                                    │
│  ) → Decision { Allow | DenyScope | DenyIdentity }                       │
│                                                                          │
│  L2 (scope):    surface_allowed(channel_id, is_dm) — default OPEN        │
│  L3 (identity): identity_allowed(sender_id)        — default DENY-ALL    │
│                                                                          │
│  On DenyIdentity: echo sender ID (with rate-limit)                       │
│  On DenyScope:    silent drop                                            │
│  On Allow:        pass event to Handler ↓                                │
│                                                                          │
│  Bot messages:    if is_bot → skip L3, still enforce L2 scope             │
│                                                                          │
│  🔑 Architectural guarantee: Handler never receives untrusted events      │
└──────────────────────────────────┬───────────────────────────────────────┘
                                   │
                                   │  Only Allow events reach here
                                   ▼
┌──────────────────────────────────────────────────────────────────────────┐
│              Handlers (per-platform interaction logic)                     │
│                                                                          │
│  ┌────────────┐  ┌────────────┐  ┌─────────────┐  ┌───────────┐        │
│  │  Discord   │  │   Slack    │  │   Gateway   │  │   Cron    │        │
│  │  Handler   │  │  Handler   │  │   Handler   │  │  Handler  │        │
│  │            │  │            │  │             │  │           │        │
│  │ • @mention │  │ • thread   │  │ • /reset    │  │ • format  │        │
│  │ • role     │  │ • assist   │  │ • /cancel   │  │   prompt  │        │
│  │ • multibot │  │   mode     │  │ • group     │  │           │        │
│  │ • reaction │  │ • emoji    │  │   routing   │  │           │        │
│  │ • channel  │  │            │  │             │  │           │        │
│  └─────┬──────┘  └─────┬──────┘  └──────┬──────┘  └─────┬─────┘        │
│        │               │                │               │              │
└────────┼───────────────┼────────────────┼───────────────┼──────────────┘
         │               │                │               │
         ▼               ▼                ▼               ▼
┌──────────────────────────────────────────────────────────────────────────┐
│                    Dispatcher → dispatch_batch() → ACP Session            │
└──────────────────────────────────────────────────────────────────────────┘
```

> **Per-platform note on "group routing":** WeCom's callback mode (self-built
> app) is **DM-only** — `channel_type` is always `"direct"` with a per-user
> channel id (`wecom:{corp}:{user}`), so group routing does not exist for WeCom
> today and its L2 scope is effectively `allow_dm` only (`allowed_channels`
> cannot match anything meaningful). Group delivery would arrive only via the
> separate WS-bot ("智能机器人") model. (Verified in
> [canyugs/openab#18](https://github.com/canyugs/openab/issues/18).)

### InboundEvent (Receiver output / Trust Gate input)

**Gateway Receiver note:** The Gateway Receiver is a **single receiver** that
connects to the openab-gateway WebSocket and **demultiplexes by platform**. Each
incoming `GatewayEvent` carries a `platform` field (e.g. `"telegram"`, `"line"`,
`"feishu"`); the Receiver uses this to populate `InboundEvent.platform`. It does
NOT spawn per-platform receivers — there is one WS connection, one event loop,
producing `InboundEvent`s tagged with the correct platform. The Trust Gate then
routes the decision to the right platform's `TrustConfig`.

**Cross-crate boundary:** The Gateway Receiver is actually **two stages** across
two crates:
1. **`openab-gateway` (edge crate):** receives platform webhooks, performs L1
   authentication, normalizes to `GatewayEvent`, forwards over WebSocket. This
   crate has NO dependency on `openab-core` and does NOT construct `InboundEvent`.
2. **`openab-core` (core crate):** receives `GatewayEvent` over WebSocket,
   wraps it into `InboundEvent { raw: RawPlatformEvent::Gateway(gw_event) }`,
   then feeds the Trust Gate.

This means `InboundEvent` and `GatedEvent` types live in `openab-core` only.
The gateway crate never sees them — it only produces `GatewayEvent` (a simpler
struct defined in a shared types crate or serialized as JSON over the WS).

```rust
/// Unified inbound event produced by all Receivers.
/// Contains the minimum fields needed for trust evaluation.
pub struct InboundEvent {
    pub platform: String,           // "discord", "telegram", "line", etc.
                                    // INVARIANT: always lowercase. Receivers MUST
                                    // normalize to lowercase before constructing.
                                    // This ensures gate_event's `== "cron"` check
                                    // and PlatformTrustConfigs::get()'s to_lowercase()
                                    // are consistent.
    pub sender_id: String,          // platform-specific sender identifier
    pub channel_id: String,         // conversation surface
    pub workspace_id: Option<String>, // workspace/team context (Slack Enterprise Grid: team_id)
    pub is_dm: bool,                // DM vs group/channel
    pub is_bot: bool,               // bot-originated message
    pub raw: RawPlatformEvent,      // opaque; Handler interprets this
}
```

**`workspace_id` usage:** Only populated when the platform has multi-workspace
semantics (currently: Slack Enterprise Grid). Used by `PlatformTrustConfigs::decide()`
to scope trust lookups and by the echo rate-limiter to key per-workspace. For
single-workspace Slack apps and all other platforms, this is `None`.

### Per-platform TrustConfig

```rust
pub struct TrustConfig {
    // L2 — scope control (NOT security). Defaults OPEN.
    pub allow_all_channels: bool,           // default true
    pub allowed_channels: HashSet<String>,
    pub allow_dm: bool,                      // default true (DM surface open)

    // L3 — identity trust (THE security gate). Defaults DENY-ALL.
    pub allow_all_users: bool,               // explicit opt-in, default false
    pub allowed_users: HashSet<String>,
}

impl TrustConfig {
    /// L2: is this conversation surface in scope? (default-open)
    pub fn surface_allowed(&self, channel_id: &str, is_dm: bool) -> bool {
        if is_dm {
            return self.allow_dm;
        }
        self.allow_all_channels || self.allowed_channels.contains(channel_id)
    }

    /// L3: is this identity trusted? (default-deny)
    pub fn identity_allowed(&self, sender_id: &str) -> bool {
        if sender_id.is_empty() { return false; }  // fail-closed on empty ID
        self.allow_all_users || self.allowed_users.contains(sender_id)
    }

    /// Combined decision: L2 then L3.
    pub fn decide(&self, channel_id: &str, is_dm: bool, sender_id: &str) -> Decision {
        if !self.surface_allowed(channel_id, is_dm) {
            return Decision::DenyScope;
        }
        if !self.identity_allowed(sender_id) {
            return Decision::DenyIdentity;
        }
        Decision::Allow
    }
}

/// Decision outcome.
#[non_exhaustive]
pub enum Decision {
    Allow,
    DenyScope,       // silent drop (L2 — not a security failure)
    DenyIdentity,    // echo sender ID (L3 — request-access UX)
}
```

### LINE group policy (platform-specific extension to `decide()`)

LINE groups have a platform-specific challenge: `sender_id` may be `"unknown"`
when a user's privacy settings prevent LINE from revealing their identity. To
handle this, LINE's trust config extends the base `TrustConfig` with a per-group
policy:

```toml
[line]
allowed_users = ["Uaaa", "Ubbb"]
default_group_policy = "members"          # fail-closed default

[[line.groups]]
id = "C1234567890"
policy = "open"         # group-level trust: any @mention sender is allowed
                        # "unknown" senders permitted; audit at group level only

[[line.groups]]
id = "C0987654321"
policy = "members"      # per-user trust: must be in allowed_users
                        # "unknown" senders denied; full per-user audit
```

**Decision logic for LINE (extends base `decide()`):**
- **1:1 DM:** `sender_id == "unknown"` → Deny; otherwise check `allowed_users`
- **Group not in configured groups** → apply `default_group_policy` (fail-closed
  default: `"members"` — same as explicitly configured `policy = "members"`)
- **Group `policy = "open"`** → Allow (group-level trust — `"unknown"` permitted)
- **Group `policy = "members"`** → `sender_id == "unknown"` → Deny; otherwise
  check `allowed_users`

**Operator tradeoff (documented):** choosing `"open"` means accepting that:
1. Anonymous/unidentifiable users can use the bot in that group
2. Audit logs only reach group level (no per-user tracking)

If per-user audit is required, use `"members"` policy.

### PlatformTrustConfigs (registry)

```rust
/// Platform-specific trust config — supports base config and platform extensions.
pub enum PlatformTrustConfig {
    /// Standard config (Discord, Telegram, Feishu, WeCom, Google Chat, Teams).
    Base(TrustConfig),
    /// LINE — extends base with group policy (open/members/unknown handling).
    Line(LineTrustConfig),
    /// Slack — extends base with workspace-scoped trust for Enterprise Grid.
    Slack(SlackTrustConfig),
}

/// LINE-specific trust config with per-group policy.
pub struct LineTrustConfig {
    pub base: TrustConfig,
    pub default_group_policy: GroupPolicy,           // "members" (fail-closed default)
    pub groups: HashMap<String, GroupPolicy>,        // group_id → policy
}

/// Slack-specific trust config with workspace-scoped identity.
pub struct SlackTrustConfig {
    pub base: TrustConfig,
    /// For Enterprise Grid: per-workspace allowed_users override.
    /// If empty, falls back to base.allowed_users (single-workspace mode).
    pub workspace_users: HashMap<String, HashSet<String>>,  // team_id → allowed user IDs
}

#[derive(Clone, Copy, PartialEq)]
pub enum GroupPolicy {
    Open,       // group-level trust — "unknown" senders permitted
    Members,    // per-user trust — "unknown" senders denied
}

pub struct PlatformTrustConfigs {
    configs: HashMap<String, PlatformTrustConfig>,  // keyed by lowercase platform name
}

impl PlatformTrustConfigs {
    /// Main decision entry point. Dispatches to platform-specific logic.
    pub fn decide(
        &self,
        platform: &str,
        channel_id: &str,
        is_dm: bool,
        sender_id: &str,
        workspace_id: Option<&str>,
    ) -> Decision {
        match self.get(platform) {
            PlatformTrustConfig::Base(c) => c.decide(channel_id, is_dm, sender_id),
            PlatformTrustConfig::Line(c) => c.decide(channel_id, is_dm, sender_id),
            PlatformTrustConfig::Slack(c) => c.decide(channel_id, is_dm, sender_id, workspace_id),
        }
    }

    /// Lookup helper for gate_event — returns the config for a platform.
    /// Returns a static deny-all default for unknown platforms.
    pub fn get(&self, platform: &str) -> &PlatformTrustConfig {
        static DEFAULT: std::sync::LazyLock<PlatformTrustConfig> =
            std::sync::LazyLock::new(|| PlatformTrustConfig::Base(TrustConfig {
                allow_all_channels: true,
                allowed_channels: HashSet::new(),
                allow_dm: true,
                allow_all_users: false,
                allowed_users: HashSet::new(),
            }));
        self.configs
            .get(&platform.to_lowercase())
            .unwrap_or(&DEFAULT)
    }
}

impl PlatformTrustConfig {
    /// Delegating method — dispatches surface_allowed to the inner TrustConfig.
    pub fn surface_allowed(&self, channel_id: &str, is_dm: bool) -> bool {
        match self {
            PlatformTrustConfig::Base(c) => c.surface_allowed(channel_id, is_dm),
            PlatformTrustConfig::Line(c) => c.base.surface_allowed(channel_id, is_dm),
            PlatformTrustConfig::Slack(c) => c.base.surface_allowed(channel_id, is_dm),
        }
    }
}

impl LineTrustConfig {
    pub fn decide(&self, channel_id: &str, is_dm: bool, sender_id: &str) -> Decision {
        // L2 scope check first (same as base)
        if !self.base.surface_allowed(channel_id, is_dm) {
            return Decision::DenyScope;
        }

        // 1:1 DM — standard identity check, but "unknown" always denied
        if is_dm {
            if sender_id == "unknown" || sender_id.is_empty() {
                return Decision::DenyIdentity;
            }
            return if self.base.identity_allowed(sender_id) {
                Decision::Allow
            } else {
                Decision::DenyIdentity
            };
        }

        // Group — look up per-group policy
        let policy = self.groups
            .get(channel_id)
            .copied()
            .unwrap_or(self.default_group_policy);

        match policy {
            GroupPolicy::Open => Decision::Allow,  // group-level trust, "unknown" permitted
            GroupPolicy::Members => {
                if sender_id == "unknown" || sender_id.is_empty() {
                    Decision::DenyIdentity
                } else if self.base.identity_allowed(sender_id) {
                    Decision::Allow
                } else {
                    Decision::DenyIdentity
                }
            }
        }
    }
}

impl SlackTrustConfig {
    pub fn decide(
        &self,
        channel_id: &str,
        is_dm: bool,
        sender_id: &str,
        workspace_id: Option<&str>,
    ) -> Decision {
        // L2 scope check
        if !self.base.surface_allowed(channel_id, is_dm) {
            return Decision::DenyScope;
        }

        // L3 identity — workspace-scoped for Enterprise Grid
        if sender_id.is_empty() { return Decision::DenyIdentity; }

        // 1. If workspace_users has an entry for this workspace, use STRICT
        //    per-workspace check — ignore allow_all_users for this workspace.
        //    Rationale: if an operator configured workspace_users for a specific
        //    workspace, they expect it to be enforced. allow_all_users only
        //    applies to workspaces without explicit workspace_users entries.
        if let Some(ws_id) = workspace_id {
            if let Some(ws_users) = self.workspace_users.get(ws_id) {
                return if ws_users.contains(sender_id) {
                    Decision::Allow
                } else {
                    Decision::DenyIdentity
                };
            }
        }

        // 2. Fallback to base config (single-workspace or unscoped).
        //    Supports both plain sender_id AND composite "team_id:sender_id"
        //    format for Enterprise Grid deployments that cannot use enterprise_user.id.
        let allowed = if let Some(ws_id) = workspace_id {
            let trust_key = format!("{}:{}", ws_id, sender_id);
            self.base.allowed_users.contains(&trust_key) || self.base.identity_allowed(sender_id)
        } else {
            self.base.identity_allowed(sender_id)
        };

        if allowed {
            Decision::Allow
        } else {
            Decision::DenyIdentity
        }
    }
}
```

**Slack Enterprise Grid config example:**

```toml
# Single-workspace Slack (most deployments):
[slack]
allowed_users = ["U01ABCDEFGH", "U09XYZWVUTS"]

# Enterprise Grid — workspace-scoped (optional):
[slack]
allowed_users = ["E0123456789"]           # enterprise_user.id (cross-workspace)

[slack.workspace_users]
T012345 = ["U01ABCDEFGH", "U01IJKLMNOP"]  # workspace-specific overrides
T067890 = ["U09XYZWVUTS"]
```

### Bot message handling

Bot messages (where `InboundEvent.is_bot == true`) **bypass L3** at the Trust Gate.
Bot admission is NOT part of the identity trust model — it is platform-specific
structural logic (e.g. `trusted_bot_ids`, `allow_bot_messages`) that stays in the
Handler. The Trust Gate only evaluates human sender identity.

**`is_bot` per-platform derivation (pinned — each Receiver must use these rules):**

| Platform | `is_bot` derivation | Notes |
|----------|-------------------|-------|
| Discord | `message.author.bot` flag | Native field from Discord API |
| Slack | `event.bot_id.is_some() \|\| event.subtype == "bot_message"` | Plus `USLACKBOT` always treated as bot |
| LINE | Always `false` | LINE has no bot-to-bot webhook delivery; bot-bypass is a no-op |
| Feishu | `trusted_bot_ids.contains(sender_open_id)` | Feishu marks other bots as `sender_type="user"` — unreliable; must match against known bot IDs |
| Telegram | `message.from.is_bot` flag | Native field from Telegram API |
| WeCom | `trusted_bot_ids.contains(userid)` | WeCom has no reliable native bot flag; match against known bot IDs. Note: `enter_agent` (member-enter event) is user-initiated — do NOT treat as bot. **Today the Receiver hardcodes `is_bot = false`** (`wecom.rs`), so the L3 bot-bypass is a no-op for WeCom until bot detection is implemented — the bypass is *available* uniformly but *effective* only where the Receiver can derive `is_bot` |
| Google Chat | `message.sender.type == "BOT"` | Native field from Chat API event payload |
| MS Teams | `activity.from.role == "bot"` or `trusted_bot_ids.contains(activity.from.id)` | Bot Framework marks bot senders with role field; verify against known bot IDs for reliability |
| LINE WORKS | Always `false` | Message callbacks deliver user messages only (no bot-to-bot delivery, same as LINE); bot-bypass is a no-op |

**`trusted_bot_ids` is shared config (NOT Handler-only):**

`trusted_bot_ids` is readable by **all layers** with different purposes:
- **Receiver:** reads the list to compute `InboundEvent.is_bot` (especially for
  Feishu where platform signals are unreliable)
- **Trust Gate:** reads `is_bot` flag to bypass L3 (does not need the list itself)
- **Handler:** reads the list for bot admission (which specific bots are allowed
  to trigger the agent vs. ignored)

This resolves the Feishu circular dependency: the Receiver can compute `is_bot`
because it has access to the config, and the Handler independently uses the same
config for admission decisions.

**Implementation note:** The `is_bot` bypass is implemented at the **Trust Gate
caller level**, not inside `TrustConfig::decide()`. This keeps `decide()` a pure
L2+L3 function with no bot-awareness:

```rust
// Trust Gate layer (pseudocode) — lives in crate::trust::gate
async fn gate_event(event: InboundEvent, configs: &PlatformTrustConfigs) -> Option<GatedEvent> {
    // System-initiated events (e.g., Cron) bypass trust entirely.
    // Cron is internal — it has no external sender and targets arbitrary
    // platform channels. Gating it against platform L2 scope would incorrectly
    // block scheduled jobs to channels not in the human-facing allowlist.
    // Only `platform` is checked — the Cron Receiver is the sole producer of
    // events with platform == "cron", so both fields are trustworthy by construction.
    // sender_id is NOT checked here to avoid spoofing: some platforms (e.g. WeCom)
    // allow freeform UserIDs that could match any synthetic value.
    if event.platform == "cron" {
        return Some(seal(event));  // fully trusted, no L2/L3
    }

    // Bot bypass — skip L3 identity check, but STILL enforce L2 scope.
    // Bots must respect channel/DM scope (noise/cost control) even though
    // they don't need identity trust.
    if event.is_bot {
        let config = configs.get(&event.platform);
        if !config.surface_allowed(&event.channel_id, event.is_dm) {
            return None;  // DenyScope — bot in wrong channel, silent drop
        }
        return Some(seal(event));  // L2 pass, skip L3
    }

    // Human sender — full L2 + L3 evaluation
    let decision = configs.decide(
        &event.platform,
        &event.channel_id,
        event.is_dm,
        &event.sender_id,
        event.workspace_id.as_deref(),
    );
    match decision {
        Decision::Allow => Some(seal(event)),
        Decision::DenyIdentity => { echo_sender_id(&event).await; None }
        Decision::DenyScope => None,  // silent drop
    }
}
```

**Note on ownership:** `gate_event` takes `InboundEvent` **by value** (ownership
transfer). On `Allow`, the event is moved into `GatedEvent` with zero copying.
On `Deny`, the event is dropped. This avoids deep-cloning `RawPlatformEvent`
(which may contain the full platform payload) on the hot path.

**Cron bypass rationale:** Cron tasks are system-initiated (no external sender).
They use `platform = "cron"` and `sender_id = "openab-cron"`. The Trust Gate
recognizes them by **`platform == "cron"` only** — the `sender_id` field is not
part of the bypass condition. This is critical because some platforms (notably
WeCom) allow tenant-admin-assigned freeform UserIDs that could match any string,
including `"openab-cron"`. Since the Cron Receiver is the sole code path that
produces events with `platform = "cron"`, checking the platform field alone is
both necessary and sufficient. The CronHandler then routes the dispatched event
to the appropriate target platform/channel.

**Reserved platform validation (MUST-level requirement):**

`"cron"` (and any future internal platform names) are **reserved**. The following
invariants MUST hold:

| Requirement | Detail |
|---|---|
| Reserved platform rejection | All externally-sourced Receivers (Gateway, Discord, Slack, etc.) MUST reject events whose normalized `platform` field matches a reserved name before constructing `InboundEvent`. Reserved names: `"cron"`, `"internal"`, any value prefixed with `"openab-"`. |
| Cron bypass invariant | Trust Gate may bypass L2/L3 only for events produced by CronReceiver. External payload fields MUST NOT be sufficient to select the cron bypass path. `InboundEvent.platform` MUST be set by the Receiver from a trusted source (hardcoded adapter name or gateway routing config), never derived from external payload content. |
| Phase 1 typed provenance (SHOULD) | Phase 1 implementation SHOULD encode system vs. external provenance with a typed source enum (e.g., `InboundSource::SystemCron` / `InboundSource::External { platform }`) to make the bypass condition impossible to forge at the type level. |

### Echo reply on deny

```rust
// In the Trust Gate layer (not in any adapter)
if decision == Decision::DenyIdentity {
    let echo = format!(
        "⚠️ You are not in the trusted list.\nYour ID: {}\nPlease ask the admin to add you to [{}].allowed_users.",
        event.sender_id,
        event.platform,  // per-event platform, not adapter.platform()
    );
    send_echo(&event, &echo).await;
}
```

**Echo safeguards:**
- **Rate-limit:** max 1 echo per `(platform, workspace_id, sender_id)` tuple per
  5 minutes (prevents spam/DoS amplification). The `workspace_id` component
  ensures Slack Enterprise Grid users get independent rate-limit quotas per
  workspace. For platforms without workspace scoping, `workspace_id` is empty/None
  and the key effectively becomes `(platform, sender_id)`.
- **Bounded state:** rate-limit cache MUST be bounded (e.g., LRU with max 10,000
  entries + 5-minute TTL eviction). An unbounded `HashMap<sender_id, Instant>`
  would allow attackers to OOM the process by sending messages with random/spoofed
  sender IDs. Implementation: use a TTL-bounded LRU cache (e.g., `moka::sync::Cache`
  or equivalent) with `max_capacity` and `time_to_live` configured.
- **Bot exclusion:** if `is_bot` → silent deny, no echo (prevents infinite reply loops between bots)
- **Best-effort:** echo delivery is not guaranteed; this is acceptable — the echo is a UX convenience, not a security mechanism

**Platform-specific echo delivery (via platform echo trait):**

The Trust Gate makes the **decision** (Allow/Deny) in core. Actual echo delivery
is delegated to a platform-specific **echo trait implementation** — core never
calls platform APIs directly.

| Platform | Echo mechanism | Rationale |
|----------|---------------|-----------|
| Discord | DM to user | Discord guarantees DM delivery |
| Slack | `chat.postEphemeral` in-channel | Only needs `chat:write` scope; no `im:write` required; visible only to target user; no UID leak |
| LINE | Reply API only; silent drop if token expired | **Never** use Push API for deny-echo (prevents attackers from burning paid Push quota) |
| Telegram | Reply in-chat | Standard reply |
| Feishu | Reply in-chat | Standard reply |
| WeCom | Reply in-chat via `message.send` API | Uses the application's send-message endpoint; targets the source conversation |
| Google Chat | Reply in-space via `spaces.messages.create` | Replies in the same space; for DMs uses the DM space with the user |
| MS Teams | Reply in-conversation via Bot Framework `sendToConversation` | Uses the Bot Framework connector to reply in the originating conversation |
| LINE WORKS | Ordinary push send to the originating user/channel | No reply-token mechanism exists (all sends are pushes); per-sender echo throttle bounds amplification. Per-message push quota is unpublished (`?`-flagged in the platform schema) |

**Echo content by scope (leak-safe):**
- **DM / 1:1 context:** echo includes sender UID (self-serve — user forwards to admin to request access)
- **Group / channel context:** echo carries **no sender ID** — generic "not authorized, contact admin" only (prevents leaking identity in shared spaces)

**LINE-specific invariant:** LINE deny-echo uses Reply API only. If the reply
token is expired (~50s TTL) or already consumed, the echo is **silently dropped**.
Push API is never used for deny-echo — this is non-overridable (not a config knob).

### Sender ID format notes (for `allowed_users` configuration)

`InboundEvent.sender_id` is always a `String`. Each platform's native ID is
converted to its string representation. Operators must configure `allowed_users`
using the **exact format** the platform provides in event payloads:

| Platform | Native type | `allowed_users` format | Example | Gotcha |
|----------|-------------|----------------------|---------|--------|
| Discord | Snowflake (u64) | Numeric string | `"845835116920307722"` | — |
| Slack | String | U-prefix or W-prefix | `"U01ABCDEFGH"` | Enterprise Grid: see below¹ |

**¹ Slack Enterprise Grid sender ID notes:**
- Receiver MUST set `workspace_id = Some(team_id)` for Grid deployments
- Prefer `enterprise_user.id` as `sender_id` when available (stable across workspaces)
- Config formats: `allowed_users = ["E0123456789"]` (enterprise_user.id, recommended) or `"T012345:U01ABCDEFGH"` (team_id:user_id fallback for non-Grid tokens)
- Trust key = `(workspace_id, sender_id)` — see `SlackTrustConfig::decide()` above
| Telegram | Integer (i64) | Stringified integer | `"123456789"` | ⚠️ Do NOT use `@username` — only numeric ID works |
| LINE | String | U + 32 hex chars | `"U1234567890abcdef0123456789abcdef"` | — |
| Feishu | String | open_id | `"ou_xxxxxxxxxxxxxxxxxxxx"` | ⚠️ `open_id` is **per-app** — same user has different ID in different Feishu apps |
| WeCom | String | UserID (self-built apps) | `"zhangsan"` | External-contact / ISV callbacks carry `wm`/`wo`-prefixed `external_userid` or encrypted OpenUserID instead — plain UserIDs only match internal members ([official docs](https://developer.work.weixin.qq.com/document/path/92113)) |
| Google Chat | String | User resource name | `"users/123456789"` | — |
| MS Teams | String | `activity.from.id` | `"29:1abc..."` | Verify via actual event payload; may differ from AAD Object ID |

### Event loop binding (how platforms wire into the pipeline)

The ingress pipeline is a **generic function** parameterized by Receiver and
Handler. Each platform spawns one `tokio::spawn` with this pipeline. The Trust
Gate is the same code for all platforms — only Receiver and Handler differ.

```rust
/// Generic ingress pipeline — all platforms use this.
async fn run_platform<R, H>(receiver: R, trust: Arc<PlatformTrustConfigs>, handler: H)
where
    R: EventReceiver,       // trait: produces InboundEvent
    H: EventHandler,        // trait: consumes GatedEvent
{
    let mut rx = receiver.start().await;

    loop {
        let event: InboundEvent = match rx.recv().await {
            Some(e) => e,
            None => break,  // connection closed / shutdown
        };

        // 🔒 Trust Gate (unified for all platforms) — takes ownership of event
        let gated = match gate_event(event, &trust).await {
            Some(g) => g,
            None => continue,  // denied (event dropped)
        };

        // Handler only receives GatedEvent (compile-time enforced)
        handler.handle(gated).await;
    }
}
```

**Traits:**

```rust
#[async_trait]
trait EventReceiver {
    /// Start listening, return a channel of normalized events.
    async fn start(&self) -> mpsc::Receiver<InboundEvent>;
}

#[async_trait]
trait EventHandler {
    /// Process a trusted event (GatedEvent — can only come from Trust Gate).
    async fn handle(&self, event: GatedEvent);
}
```

**Startup wiring (`main.rs`):**

```rust
async fn main() {
    let trust = Arc::new(PlatformTrustConfigs::from_config(&config));
    let dispatcher = Arc::new(Dispatcher::new(...));

    // Discord — own WebSocket connection
    if config.discord.enabled {
        tokio::spawn(run_platform(
            DiscordReceiver::new(config.discord.clone()),
            trust.clone(),
            DiscordHandler::new(config.discord, dispatcher.clone()),
        ));
    }

    // Slack — own Socket Mode connection
    if config.slack.enabled {
        tokio::spawn(run_platform(
            SlackReceiver::new(config.slack.clone()),
            trust.clone(),
            SlackHandler::new(config.slack, dispatcher.clone()),
        ));
    }

    // Gateway platforms — ONE shared WebSocket, demux by platform
    if config.has_gateway_platforms() {
        tokio::spawn(run_gateway_platforms(
            config.gateway_url.clone(),
            trust.clone(),
            dispatcher.clone(),
            config.clone(),
        ));
    }

    // Cron — timer-based
    if config.cron.enabled {
        tokio::spawn(run_platform(
            CronReceiver::new(config.cron.clone()),
            trust.clone(),
            CronHandler::new(config.cron, dispatcher.clone()),
        ));
    }
}
```

**Gateway platforms — one WebSocket, fan-out by platform:**

Gateway-connected platforms (Telegram, LINE, Feishu, WeCom, Google Chat, Teams)
share a **single WebSocket** connection to `openab-gateway`. The gateway has
already performed L1 authentication and normalized events into `GatewayEvent`.
Core receives all events on one connection and **demuxes by `event.platform`**:

```rust
async fn run_gateway_platforms(
    url: String,
    trust: Arc<PlatformTrustConfigs>,
    dispatcher: Arc<Dispatcher>,
    config: Config,
) {
    let mut ws = connect_to_gateway(&url).await;

    loop {
        let gw_event: GatewayEvent = ws.recv().await;

        // Normalize GatewayEvent → InboundEvent
        // Note: gw_event.platform is assigned by gateway routing config (based on
        // which webhook endpoint received the request), NOT copied from the platform
        // webhook payload body. This satisfies the reserved platform invariant.
        let event = InboundEvent {
            platform: gw_event.platform.clone(),
            sender_id: gw_event.sender_id.clone(),
            channel_id: gw_event.channel_id.clone(),
            workspace_id: None,  // gateway platforms don't use workspace scoping
            is_dm: gw_event.is_dm,
            is_bot: gw_event.is_bot,
            raw: RawPlatformEvent::Gateway(gw_event.clone()),
        };

        // 🔒 Trust Gate (same logic, keyed by event.platform) — takes ownership
        let gated = match gate_event(event, &trust).await {
            Some(g) => g,
            None => continue,
        };

        // Fan-out to platform-specific Handler
        match gated.platform() {
            "telegram"   => telegram_handler.handle(gated).await,
            "line"       => line_handler.handle(gated).await,
            "feishu"     => feishu_handler.handle(gated).await,
            "wecom"      => wecom_handler.handle(gated).await,
            "googlechat" => googlechat_handler.handle(gated).await,
            "teams"      => teams_handler.handle(gated).await,
            unknown      => warn!("unknown gateway platform: {unknown}"),
        }
    }
}
```

**Summary of binding topology:**

```
Discord   → own WS     → DiscordReceiver  → 🔒 Trust Gate → DiscordHandler
Slack     → own WS     → SlackReceiver    → 🔒 Trust Gate → SlackHandler
Gateway   → one WS     → GatewayReceiver  → 🔒 Trust Gate → demux by platform
              (shared)                                          ├→ TelegramHandler
                                                               ├→ LineHandler
                                                               ├→ FeishuHandler
                                                               ├→ WecomHandler
                                                               ├→ GoogleChatHandler
                                                               └→ TeamsHandler
Cron      → timer      → CronReceiver     → 🔒 Trust Gate → CronHandler
```

**Design choice — one WS for all gateway platforms (not per-platform):**
- Matches current architecture (minimal change)
- Gateway already normalizes all platform events into `GatewayEvent`
- One connection = one reconnect logic, one heartbeat, one backpressure
- Trust Gate uses `event.platform` to look up the correct per-platform config
- If the WS drops, all gateway platforms go offline together — acceptable,
  same as current behavior, and gateway is designed for high availability

## 6. Migration

### Phased rollout (not a hard cutover)

The default flip is phased to avoid silently severing live bots on upgrade:

| Phase | Behavior | When |
|-------|----------|------|
| **Phase 0** | Types + `decide()` defined, no runtime behavior change. Additive only. | Done (on main) |
| **Phase 0.5** | Partial wiring using the existing adapter structure (not yet the Receiver/Handler split); trust checks co-exist with scattered per-adapter checks during transition. Shipped: `AdapterRouter::gate_incoming` on the unified gateway path, Discord L3 gate (#1270), Slack L3 gate (#1363), per-platform `[section]` trust for all 8 platforms replacing the uniform `GATEWAY_*` seed (#1297, #1365, #1366, #1385), full config-first parity incl. credentials (#1375: #1381–#1385 + conformance guard #1387), and the L1 unenforceable-auth startup warning (#1373). Outstanding for Phase 1: the standalone-gateway WS path still enforces via `should_skip_event` (not the shared gate) — tracked on #1356. | Done (on main) |
| **Phase 1** | Complete Receiver/Handler split. Wire Trust Gate as the sole ingress layer. **Keep current allow-all default.** Log deprecation warning when relying on implicit allow-all. Remove scattered checks. | Next release |
| **Phase 2** | Require explicit `allow_all_users = true` to preserve old behavior. Deployments without it get a **startup error** (not silent denial — bot refuses to start, operator must explicitly choose). | Pre-GA release |
| **Phase 3** | Flip default: empty `allowed_users` + no `allow_all_users` = **deny-all**. | GA release |

### Migration path

```toml
# Before (implicit trust-all — works in Phase 0/1, warns in Phase 1, errors in Phase 2):
[discord]
bot_token = "..."

# After (explicit trust-all to keep old behavior across all phases):
[discord]
bot_token = "..."
allow_all_users = true

# Or (recommended — actually configure trust):
[discord]
bot_token = "..."
allowed_users = ["845835116920307722"]
```

### `[gateway]` vs first-class section precedence

When both a deprecated `[gateway]` section and a matching first-class section
(e.g. `[telegram]`) exist in config, the **first-class section wins**. The
`[gateway]` entry for that platform is ignored and a deprecation warning is
logged at startup. If only `[gateway]` exists for a platform, it remains
functional.

## 7. Implementation Plan

1. **Define `InboundEvent`** — unified event struct that all Receivers produce
2. **Refactor adapters into Receiver + Handler** — starting with Discord and
   Gateway (Telegram). The Receiver produces `InboundEvent`; the Handler consumes
   only events that passed the Trust Gate.
3. **Wire Trust Gate as the ingress layer** between Receiver and Handler:
   - Receives `InboundEvent` from Receiver
   - Calls `PlatformTrustConfigs::decide(event.platform, ...)`
   - Passes allowed events to Handler
   - Echoes + drops denied events
4. **Remove scattered trust checks** — replaced by the unified Trust Gate:
   - `is_denied_user()` call sites in Discord `EventHandler` (forum-post, DM,
     and guild-message paths)
   - `should_skip_event()` call sites in `run_gateway_adapter` and
     `process_gateway_event` (gateway.rs)
   - Inline `allowed_users` check in Slack `should_process_message()`
   - `allowed_users` + `allowed_groups` filters in Feishu `parse_message_event()`
     (gateway crate) — must relocate to core Trust Gate, not just delete
     (contradicts "gateway = L1 only" model)
     **Feishu double-gating elimination:** Currently Feishu identity is checked
     twice — `FEISHU_ALLOWED_USERS`/`FEISHU_ALLOWED_GROUPS` in the gateway crate
     AND `[gateway].allowed_users` in core. These can diverge, and the core side
     **fails open** when its list is empty (`resolve_allow_all = list.is_empty()`).
     Resolution: gateway crate performs L1 only (signature + decrypt); all
     `allowed_users` / `allowed_groups` checks move to core Trust Gate. Empty
     list = **deny-all** (requires explicit `allow_all_users = true` to open).
     Gateway env vars (`FEISHU_ALLOWED_USERS`, `FEISHU_ALLOWED_GROUPS`) are
     deprecated in Phase 1 (warn), conflict-error in Phase 2, removed in Phase 3.
   - Discord reaction-dispatch gating in `EventHandler`
   - Note: `trusted_bot_ids`, `allow_bot_messages`, `allowed_role_ids` **stay in
     Handlers** — they are structural/trigger semantics, not identity trust.
5. **Add echo reply with safeguards** — rate-limit, bot exclusion, DM-preferred
6. **Structured logging** — log sender_id + platform on both deny AND allow
   (existing dispatch logs use sender name; add structured sender_id field)
7. **Update `config.toml.example`** and docs; migration guide in release notes

### What stays in Handlers (NOT moved to Trust Gate)

These are platform-specific structural concerns, not trust:
- Thread detection and routing
- @mention gating and multibot detection
- Bot-ownership and `trusted_bot_ids`
- `allowed_role_ids` (Discord role-based trigger control)
- Reaction dispatch gating (triggers, not authorization)
- Slash command routing (`/reset`, `/cancel`) — but note these now run AFTER the
  Trust Gate, so untrusted senders cannot invoke them.
  **Scope note:** "slash commands are gated" applies to **gateway-platform
  commands** (Telegram `/reset`, `/cancel`) implemented as text-prefix detection
  in the Handler. The **Slack adapter does not consume `slash_commands` or
  `interactive` envelopes** — thread routing cannot be reconstructed for them.
  If Slack slash command support is added in the future, those events must flow
  through the full Receiver → Trust Gate → Handler pipeline.

### Non-message events that MUST flow through the Trust Gate

Any event type that can **trigger agent state changes** must flow through the
full Receiver → Trust Gate → Handler pipeline. The Trust Gate is not limited to
`message` events:

| Event | Platform | Must gate? | `InboundEvent` mapping |
|-------|----------|-----------|----------------------|
| `assistant_thread_started` | Slack | **Yes** — untrusted user can establish agent state | `is_dm: true`, `sender_id: event.user` |
| `assistant_thread_context_changed` | Slack | **Yes** — modifies thread context | `is_dm: true`, `sender_id: event.user` |
| `reaction_added` | Slack | Not implemented today | Future: reactor identity (`event.user`) must pass L3 if reactions trigger agent actions |
| Message events | All | **Yes** | Standard mapping |

**Rule:** if a new event type is added to any Receiver and it can cause the agent
to execute, store state, or respond, its sender identity **must** pass through L3.
Events that are purely informational (e.g. typing indicators) may be excluded.

### Slack-specific scope notes

- **L1 authentication:** Slack L1 = Socket Mode `app_token` verification. Events
  API HTTP mode (`X-Slack-Signature` HMAC) is out of scope for this ADR.
- **MPIM classification:** `D`-prefix channels = DM (`is_dm = true`). `G`-prefix
  channels (MPIM / group DMs) are treated as **channels** (`is_dm = false`) — they
  behave more like channels (multiple participants, shared context). If operators
  need MPIM-as-DM semantics, a future enhancement can add `conversations.info`
  lookup.

## 8. Rejected Alternatives

### Per-adapter `InboundGate` trait

Each adapter implements `is_trusted_sender()`. Rejected because:
- Trust logic is identical across all platforms (`allowed_users.contains(id)`)
- Forces N identical implementations with no polymorphic benefit
- New adapter forgetting to implement = security hole
- Three-layer architecture makes accidental bypass a compile error by construction

### Trust check at gateway layer

Gateway adapters filter untrusted senders before forwarding. Rejected because:
- Gateway is transport (L1) — mixing L3 policy violates layer separation
- Trust config lives in core's `config.toml`, not gateway env vars
- Reply capability already wired in core via `ChatAdapter::send_message()`

### Trust gate inside Dispatcher::submit() (downstream of adapter)

Wire gate into `Dispatcher::submit()` or `AdapterRouter::handle_message()`.
Rejected because:
- Gate is downstream of the Handler — Handler still receives untrusted events
- Slash commands (`/reset`, `/cancel`) processed in the Handler would execute
  before the gate is reached
- Does not provide the architectural guarantee that untrusted events are invisible
  to platform-specific logic
- The "by construction" safety property requires the gate to be UPSTREAM of any
  platform-specific code that acts on events

### Treating L2 (channel) as a security layer

Rejected: the platform already enforces channel/group membership, so L2 is
operator scoping, not authorization. Modeling it as security would wrongly imply
DMs are protected by channel rules — they are not (a DM has no membership gate;
only L3 protects it).

### L2 default-closed

Rejected: closing surfaces by default breaks the echo/request-access onboarding
flow (an untrusted sender would be dropped before reaching L3 and never learn how
to request access).

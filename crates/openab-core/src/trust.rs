//! Shared trust model — the L2 (scope) + L3 (identity) layers of the trust
//! pyramid (see ADR: identity trust-none default & trust pyramid).
//!
//! Phase 0 (this module) is **purely additive**: it defines the shared
//! [`TrustConfig`] / [`PlatformTrustConfigs`] types and the pure decision
//! function. It is NOT yet wired into `AdapterRouter::handle_message()` and does
//! not change any runtime behavior. Wiring (and removing the scattered per-adapter
//! checks) lands in Phase 1; the trust-none default flip lands in Phase 3.
//!
//! Layering recap:
//! - **L2 — scope control** (`allow_all_channels` / `allowed_channels` / `allow_dm`):
//!   which conversation *surfaces* the bot engages in. NOT a security boundary —
//!   the platform already enforces channel membership. **Default: open.**
//! - **L3 — identity trust** (`allow_all_users` / `allowed_users`): which *human*
//!   senders may trigger the agent. The security gate. **Default: deny-all.**
//!
//! Bot admission (`trusted_bot_ids` / `allow_bot_messages`) and trigger semantics
//! (@mention, multibot, role triggers) are intentionally NOT part of this model —
//! they stay in the adapters.

use std::collections::HashSet;

/// Outcome of evaluating the trust gate for a single inbound message.
///
/// `#[non_exhaustive]` because later phases may add variants (e.g. a
/// rate-limited/throttled echo state); callers must include a `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Decision {
    /// Allowed — dispatch to the agent.
    Allow,
    /// Denied at L2 (scope): the bot is not configured to operate on this
    /// conversation surface. This is **scope control, not an authorization
    /// failure** (L2 is not a security boundary) — so it is silent (no echo).
    DenyScope,
    /// Denied at L3 (identity): the surface is in scope but the sender is not
    /// trusted. The caller should echo the sender their ID (request-access UX).
    DenyIdentity,
}

impl Decision {
    /// Whether the router should echo the sender their ID on this decision.
    /// Only L3 (identity) denials get the request-access echo.
    pub fn should_echo(self) -> bool {
        matches!(self, Decision::DenyIdentity)
    }

    pub fn is_allowed(self) -> bool {
        matches!(self, Decision::Allow)
    }
}

/// Whether the shared L3 identity gate (`AdapterRouter::gate_incoming`) should
/// run for this sender. Bots bypass L3 — mirroring the adapters' inline user
/// checks' `!is_bot` bypass — because bot admission is a separate concern
/// (`allow_bot_messages` + `trusted_bot_ids`), and L3 (`allowed_users`) is a
/// human-identity allowlist. Running L3 on bots would wrongly deny
/// mode-admitted/trusted bots when `allow_all_users=false` (multi-agent).
/// See PR #1270 review F1; shared by the Discord and Slack gate call sites.
pub fn l3_gate_applies(is_bot: bool) -> bool {
    !is_bot
}

/// Per-platform trust configuration (L2 scope + L3 identity).
///
/// Construct via [`TrustConfig::new`], which applies the ADR defaults:
/// **L2 open, L3 deny-all**. Fields are public for cross-crate construction
/// (the binary builds the registry from config), but `new()` is the canonical
/// constructor. "Inconsistent" combinations are benign by precedence: an
/// `allow_all_*` flag always wins, so e.g. `allow_all_channels = true` with a
/// non-empty `allowed_channels` simply ignores the list.
#[derive(Debug, Clone)]
pub struct TrustConfig {
    // --- L2: scope control (NOT security). Default open. ---
    pub allow_all_channels: bool,
    pub allowed_channels: HashSet<String>,
    pub allow_dm: bool,
    // --- L3: identity trust (security gate). Default deny-all. ---
    pub allow_all_users: bool,
    pub allowed_users: HashSet<String>,
}

impl Default for TrustConfig {
    /// L2 open, L3 deny-all — the ADR's default posture.
    fn default() -> Self {
        Self {
            allow_all_channels: true,
            allowed_channels: HashSet::new(),
            allow_dm: true,
            allow_all_users: false,
            allowed_users: HashSet::new(),
        }
    }
}

impl TrustConfig {
    /// Build from raw config values, applying defaults for unset flags:
    /// - L2 `allow_all_channels` / `allow_dm` default **true** (open)
    /// - L3 `allow_all_users` defaults **false** (deny-all)
    ///
    /// NOTE: this is the ADR-correct (Phase 3) resolution. Phase 0/1 do not call
    /// this at runtime, so shipping it here changes no behavior yet.
    pub fn new(
        allow_all_channels: Option<bool>,
        allowed_channels: impl IntoIterator<Item = String>,
        allow_dm: Option<bool>,
        allow_all_users: Option<bool>,
        allowed_users: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            allow_all_channels: allow_all_channels.unwrap_or(true),
            allowed_channels: allowed_channels.into_iter().collect(),
            allow_dm: allow_dm.unwrap_or(true),
            allow_all_users: allow_all_users.unwrap_or(false),
            allowed_users: allowed_users.into_iter().collect(),
        }
    }

    /// L2: is this conversation surface in scope?
    /// DMs are gated by `allow_dm`; channels/groups by the channel allowlist.
    pub fn surface_allowed(&self, channel_id: &str, is_dm: bool) -> bool {
        if is_dm {
            return self.allow_dm;
        }
        self.allow_all_channels || self.allowed_channels.contains(channel_id)
    }

    /// Phase 6.4.1E — parent-aware L2 scope check.
    ///
    /// Rule (must follow exactly):
    /// - DMs short-circuit on `allow_dm` (no parent concept).
    /// - `allow_all_channels` wins.
    /// - `channel_id ∈ allowed_channels` wins.
    /// - `parent_id ∈ allowed_channels` is a strict fallback that
    ///   applies only when `parent_id == Some(_)`.
    /// - `parent_id == None` ⇒ identical to `surface_allowed`, so every
    ///   pre-6.4.1E caller that does not yet carry a parent is bit-exact.
    ///
    /// This helper is additive: it does NOT modify `surface_allowed`,
    /// does NOT introduce a new allowlist, and does NOT parse parent
    /// from any conversation key. Parent inheritance is bounded by the
    /// operator's existing `allowed_channels`.
    pub fn surface_allowed_with_parent(
        &self,
        channel_id: &str,
        parent_id: Option<&str>,
        is_dm: bool,
    ) -> bool {
        if is_dm {
            return self.allow_dm;
        }
        self.allow_all_channels
            || self.allowed_channels.contains(channel_id)
            || parent_id.is_some_and(|p| self.allowed_channels.contains(p))
    }

    /// L3: is this (human) identity trusted?
    ///
    /// An empty `sender_id` (e.g. a system/webhook message with no human author)
    /// is **never** identity-allowed — fail-closed, even under `allow_all_users`,
    /// since an absent identity cannot be a trusted user.
    pub fn identity_allowed(&self, sender_id: &str) -> bool {
        if sender_id.is_empty() {
            return false;
        }
        self.allow_all_users || self.allowed_users.contains(sender_id)
    }

    /// Evaluate L2 (scope) then L3 (identity) and return the [`Decision`]:
    ///
    /// ```text
    ///   surface_allowed?  ──no──▶ DenyScope     (silent)
    ///        │ yes
    ///   identity_allowed? ──no──▶ DenyIdentity  (echo UID)
    ///        │ yes
    ///        ▼
    ///      Allow
    /// ```
    pub fn decide(&self, channel_id: &str, is_dm: bool, sender_id: &str) -> Decision {
        if !self.surface_allowed(channel_id, is_dm) {
            return Decision::DenyScope;
        }
        if !self.identity_allowed(sender_id) {
            return Decision::DenyIdentity;
        }
        Decision::Allow
    }

    /// Phase 6.4.1E — parent-aware L2+L3 evaluation.
    ///
    /// L2 uses `surface_allowed_with_parent`. L3 (identity) is
    /// unchanged — parent inheritance does NOT bypass `allowed_users`
    /// or the empty-sender fail-closed posture. If a parent channel is
    /// allowed but the sender is not in `allowed_users` (and
    /// `allow_all_users == false`), the decision is `DenyIdentity`
    /// (request-access echo path), not `Allow`.
    pub fn decide_with_parent(
        &self,
        channel_id: &str,
        parent_id: Option<&str>,
        is_dm: bool,
        sender_id: &str,
    ) -> Decision {
        if !self.surface_allowed_with_parent(channel_id, parent_id, is_dm) {
            return Decision::DenyScope;
        }
        if !self.identity_allowed(sender_id) {
            return Decision::DenyIdentity;
        }
        Decision::Allow
    }
}

/// Registry of per-platform [`TrustConfig`], keyed by `platform()` name
/// (e.g. "discord", "slack", "telegram"). Keying by platform prevents
/// cross-platform ID bleed (a Telegram UID can never satisfy a LINE allowlist).
#[derive(Debug, Clone, Default)]
pub struct PlatformTrustConfigs {
    map: std::collections::HashMap<String, TrustConfig>,
    default: TrustConfig,
}

impl PlatformTrustConfigs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a platform's trust config. The platform key is normalized to
    /// lowercase so a case mismatch with `adapter.platform()` can't silently
    /// fall back to the deny-all default.
    pub fn insert(&mut self, platform: impl Into<String>, cfg: TrustConfig) {
        self.map.insert(platform.into().to_lowercase(), cfg);
    }

    /// Get the trust config for a platform, or the default (L2 open / L3 deny-all)
    /// when the platform has no explicit configuration. Lookup is case-insensitive.
    pub fn get(&self, platform: &str) -> &TrustConfig {
        self.map
            .get(&platform.to_lowercase())
            .unwrap_or(&self.default)
    }

    /// Convenience: evaluate the gate for a platform in one call.
    pub fn decide(
        &self,
        platform: &str,
        channel_id: &str,
        is_dm: bool,
        sender_id: &str,
    ) -> Decision {
        self.get(platform).decide(channel_id, is_dm, sender_id)
    }

    /// Phase 6.4.1E — registry entry point for parent-aware inbound.
    /// Mirrors `decide()` but threads `parent_id` through L2. `None`
    /// parent reduces to `decide()` (bit-exact) — every pre-6.4.1E
    /// caller that already invokes `decide()` continues to behave
    /// identically.
    pub fn decide_with_parent(
        &self,
        platform: &str,
        channel_id: &str,
        parent_id: Option<&str>,
        is_dm: bool,
        sender_id: &str,
    ) -> Decision {
        self.get(platform)
            .decide_with_parent(channel_id, parent_id, is_dm, sender_id)
    }

    /// Phase 6.4.1D — outbound channel authorization. Single source
    /// of truth: reuses the existing ``TrustConfig.surface_allowed``
    /// populated from operator config (e.g.
    /// ``[platform.discord].allowed_channels``). DM gating is an
    /// inbound concern, so ``is_dm`` is hard-coded ``false`` here —
    /// outbound sends do not need to distinguish DMs from
    /// channels/threads because the field shape does not carry an
    /// ``is_dm`` flag.
    ///
    /// This replaces the parallel ``OPENAB_NATIVE_DELIVERY_ALLOWLIST``
    /// env-var policy so all outbound authorization flows through
    /// the canonical L2 trust authority.
    pub fn surface_allowed_for_outbound(&self, platform: &str, channel_id: &str) -> bool {
        self.get(platform)
            .surface_allowed(channel_id, /*is_dm=*/ false)
    }

    /// Phase 6.4.1D Round 2 (bounded correction) — outbound-only
    /// authorization that **fails closed when the platform has no
    /// explicit trust config**.
    ///
    /// Rationale: the inbound ``surface_allowed()`` (and the registry
    /// ``get()`` it delegates to) deliberately returns the registry's
    /// default ``TrustConfig`` for any unrecognised / unconfigured
    /// platform — and that default is L2-open (ADR-correct for the
    /// inbound surface, where the platform itself already enforces
    /// channel membership). Outbound is a different threat model:
    /// there is no upstream membership check, the daemon is choosing
    /// where to *write*, and a single typo / unknown platform string
    /// would silently default to allow-all.
    ///
    /// Contract:
    ///
    /// - platform key present in the registry with an explicit config
    ///   → defer to that config's ``surface_allowed`` (L2-allow-all
    ///   wins for operators that chose to leave it open; explicit
    ///   ``allowed_channels`` list restricts).
    /// - platform key absent OR unknown
    ///   → ``false``. No env-var fallback, no parallel allowlist.
    ///
    /// This is the single source of truth for outbound native delivery
    /// authorization. Both the structured ``delivery_destination``
    /// branch and the legacy ``native_delivery_target`` fallback branch
    /// in ``ctl::handle_agent_work`` route through here.
    ///
    /// Inbound behavior is **unchanged**: ``decide()`` and
    /// ``surface_allowed()`` continue to serve the L2-open / L3-deny
    /// ADR default for unconfigured platforms.
    pub fn authorize_outbound_channel(&self, platform: &str, channel_id: &str) -> bool {
        let key = platform.to_lowercase();
        match self.map.get(&key) {
            Some(cfg) => cfg.surface_allowed(channel_id, /*is_dm=*/ false),
            None => false,
        }
    }

    /// Phase 6.4.1E — parent-aware outbound authorization for the
    /// structured ``delivery_destination`` branch in
    /// ``ctl::handle_agent_work``.
    ///
    /// Mirrors ``authorize_outbound_channel`` (Round 2 fail-closed
    /// posture for unconfigured platforms) but threads ``parent_id``
    /// through L2: a Discord thread whose ``channel_id`` is not in
    /// ``allowed_channels`` is still authorized when its
    /// ``parent_id`` IS in the allowlist. The legacy
    /// ``authorize_outbound_channel`` helper is preserved for callers
    /// that do not carry a parent (e.g. the daemon-wide
    /// ``native_delivery_target`` fallback, where the static
    /// ``ChannelRef.parent_id`` is ``None`` by construction).
    ///
    /// Inbound behaviour is **unchanged**: ``decide()`` and
    /// ``decide_with_parent()`` continue to serve the L2-open /
    /// L3-deny ADR default for unconfigured platforms.
    pub fn authorize_outbound_channel_with_parent(
        &self,
        platform: &str,
        channel_id: &str,
        parent_id: Option<&str>,
    ) -> bool {
        let key = platform.to_lowercase();
        match self.map.get(&key) {
            Some(cfg) => {
                cfg.surface_allowed_with_parent(channel_id, parent_id, /*is_dm=*/ false)
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> TrustConfig {
        // L2 open, explicit allowed channel; L3 with one allowed user.
        TrustConfig::new(
            None, // allow_all_channels → true
            ["chan-1".to_string()],
            None, // allow_dm → true
            None, // allow_all_users → false (deny)
            ["user-1".to_string()],
        )
    }

    #[test]
    fn defaults_are_l2_open_l3_deny() {
        let c = TrustConfig::default();
        assert!(c.allow_all_channels);
        assert!(c.allow_dm);
        assert!(!c.allow_all_users);
        assert!(c.allowed_users.is_empty());
    }

    #[test]
    fn allowed_user_in_scope_channel_is_allowed() {
        assert_eq!(
            cfg().decide("any-channel", false, "user-1"),
            Decision::Allow
        );
    }

    #[test]
    fn untrusted_user_in_channel_denied_identity() {
        assert_eq!(
            cfg().decide("any-channel", false, "stranger"),
            Decision::DenyIdentity
        );
    }

    #[test]
    fn untrusted_user_in_dm_denied_identity_not_scope() {
        // DM surface open by default → reaches L3 → identity deny (echo path).
        assert_eq!(
            cfg().decide("dm-chan", true, "stranger"),
            Decision::DenyIdentity
        );
    }

    #[test]
    fn allowed_user_in_dm_is_allowed() {
        assert_eq!(cfg().decide("dm-chan", true, "user-1"), Decision::Allow);
    }

    #[test]
    fn scope_denied_when_channel_not_listed_and_not_open() {
        let c = TrustConfig::new(
            Some(false), // allow_all_channels closed
            ["chan-1".to_string()],
            Some(false), // allow_dm closed
            Some(true),  // allow_all_users (irrelevant — L2 fails first)
            std::iter::empty(),
        );
        // Out-of-scope channel → DenyScope (no echo), even though L3 would allow.
        assert_eq!(c.decide("other-chan", false, "anyone"), Decision::DenyScope);
        // DM closed → DenyScope.
        assert_eq!(c.decide("dm", true, "anyone"), Decision::DenyScope);
        // In-scope channel → L3 allows (allow_all_users).
        assert_eq!(c.decide("chan-1", false, "anyone"), Decision::Allow);
    }

    #[test]
    fn allow_all_users_opens_l3() {
        let c = TrustConfig::new(
            None,
            std::iter::empty(),
            None,
            Some(true),
            std::iter::empty(),
        );
        assert_eq!(c.decide("c", false, "anyone"), Decision::Allow);
    }

    #[test]
    fn dm_closed_denies_scope_even_for_allowed_user() {
        let c = TrustConfig::new(
            None,
            std::iter::empty(),
            Some(false),
            None,
            ["user-1".to_string()],
        );
        // allowed user, but DM surface disabled → DenyScope (no echo).
        assert_eq!(c.decide("dm", true, "user-1"), Decision::DenyScope);
        // same user in a channel (L2 open) → Allow.
        assert_eq!(c.decide("c", false, "user-1"), Decision::Allow);
    }

    #[test]
    fn decision_echo_semantics() {
        assert!(Decision::DenyIdentity.should_echo());
        assert!(!Decision::DenyScope.should_echo());
        assert!(!Decision::Allow.should_echo());
        assert!(Decision::Allow.is_allowed());
    }

    #[test]
    fn registry_returns_default_for_unknown_platform() {
        let reg = PlatformTrustConfigs::new();
        // unknown platform → default (L3 deny-all) → stranger denied identity.
        assert_eq!(
            reg.decide("mars", "c", false, "stranger"),
            Decision::DenyIdentity
        );
    }

    #[test]
    fn registry_uses_registered_platform_config() {
        let mut reg = PlatformTrustConfigs::new();
        reg.insert(
            "telegram",
            TrustConfig::new(None, std::iter::empty(), None, None, ["123".to_string()]),
        );
        assert_eq!(reg.decide("telegram", "c", false, "123"), Decision::Allow);
        assert_eq!(
            reg.decide("telegram", "c", false, "999"),
            Decision::DenyIdentity
        );
        // unregistered platform still gets deny-all default.
        assert_eq!(
            reg.decide("discord", "c", false, "123"),
            Decision::DenyIdentity
        );
    }

    #[test]
    fn empty_sender_is_never_identity_allowed() {
        // Even with allow_all_users = true, an empty sender_id fails closed.
        let open = TrustConfig::new(
            None,
            std::iter::empty(),
            None,
            Some(true),
            std::iter::empty(),
        );
        assert!(!open.identity_allowed(""));
        assert_eq!(open.decide("c", false, ""), Decision::DenyIdentity);
        // non-empty still allowed under allow_all_users.
        assert_eq!(open.decide("c", false, "anyone"), Decision::Allow);
    }

    #[test]
    fn registry_lookup_is_case_insensitive() {
        let mut reg = PlatformTrustConfigs::new();
        reg.insert(
            "Telegram",
            TrustConfig::new(None, std::iter::empty(), None, None, ["123".to_string()]),
        );
        // mixed-case platform() value resolves to the same config.
        assert_eq!(reg.decide("telegram", "c", false, "123"), Decision::Allow);
        assert_eq!(reg.decide("TELEGRAM", "c", false, "123"), Decision::Allow);
    }

    // ── Phase 6.4.1D Round 2 — outbound fail-closed contract tests.

    /// Round 2 — configured platform + L2-open (operator chose allow-all) →
    /// outbound passes. This is the "operator explicitly opted into
    /// allow-all" case and MUST stay open.
    #[test]
    fn authorize_outbound_open_config_allows_channel() {
        let mut reg = PlatformTrustConfigs::new();
        reg.insert(
            "discord",
            TrustConfig::new(
                Some(true), // allow_all_channels = true
                std::iter::empty::<String>(),
                Some(true),
                None,
                std::iter::empty::<String>(),
            ),
        );
        assert!(reg.authorize_outbound_channel("discord", "any-channel"));
        assert!(reg.authorize_outbound_channel("discord", "another"));
    }

    /// Round 2 — configured platform + explicit allowed_channels list,
    /// channel in list → outbound passes.
    #[test]
    fn authorize_outbound_configured_channel_in_list_passes() {
        let mut reg = PlatformTrustConfigs::new();
        reg.insert(
            "discord",
            TrustConfig::new(
                Some(false),
                ["111111111111111111".to_string()],
                Some(true),
                None,
                std::iter::empty::<String>(),
            ),
        );
        assert!(reg.authorize_outbound_channel("discord", "111111111111111111"));
    }

    /// Round 2 — configured platform + explicit allowed_channels list,
    /// channel NOT in list → outbound denied.
    #[test]
    fn authorize_outbound_configured_channel_not_in_list_denied() {
        let mut reg = PlatformTrustConfigs::new();
        reg.insert(
            "discord",
            TrustConfig::new(
                Some(false),
                ["111111111111111111".to_string()],
                Some(true),
                None,
                std::iter::empty::<String>(),
            ),
        );
        assert!(!reg.authorize_outbound_channel("discord", "222222222222222222"));
    }

    /// Round 2 — DEFECT A core fix: platform NOT in the registry →
    /// outbound denied (FAIL CLOSED), even though the inbound
    /// ``decide()`` would have returned the registry's L3-deny-all
    /// default. This is the critical bug close.
    #[test]
    fn authorize_outbound_unconfigured_platform_denied() {
        let reg = PlatformTrustConfigs::new();
        // Empty registry → no platform is configured. Inbound
        // ``decide()`` returns DenyIdentity (L3 deny-all default);
        // outbound ``authorize_outbound_channel`` MUST also return
        // false — and crucially, NOT inherit the L2-open default.
        assert!(!reg.authorize_outbound_channel("discord", "any-channel"));
        assert!(!reg.authorize_outbound_channel("slack", "C123"));
        assert!(!reg.authorize_outbound_channel("telegram", "12345"));
    }

    /// Round 2 — DEFECT A: even when one platform IS configured, an
    /// unrelated unconfigured platform is denied outbound.
    #[test]
    fn authorize_outbound_partial_config_unconfigured_platform_denied() {
        let mut reg = PlatformTrustConfigs::new();
        reg.insert(
            "discord",
            TrustConfig::new(
                Some(true), // discord L2-open
                std::iter::empty::<String>(),
                Some(true),
                None,
                std::iter::empty::<String>(),
            ),
        );
        // discord is configured → outbound passes.
        assert!(reg.authorize_outbound_channel("discord", "any-channel"));
        // slack is NOT configured → outbound fails closed.
        assert!(!reg.authorize_outbound_channel("slack", "C123"));
    }

    /// Round 2 — platform key normalisation is case-insensitive
    /// (mirrors the existing ``get()`` / ``decide()`` invariant).
    #[test]
    fn authorize_outbound_lookup_is_case_insensitive() {
        let mut reg = PlatformTrustConfigs::new();
        reg.insert(
            "Discord",
            TrustConfig::new(
                Some(false),
                ["111111111111111111".to_string()],
                Some(true),
                None,
                std::iter::empty::<String>(),
            ),
        );
        assert!(reg.authorize_outbound_channel("DISCORD", "111111111111111111"));
        assert!(reg.authorize_outbound_channel("discord", "111111111111111111"));
        // Case-insensitive lookup means ``DISCORD``/``Discord``/``discord``
        // all resolve to the same registry entry — so an unconfigured
        // ``SLACK`` (any case) still fails closed.
        assert!(!reg.authorize_outbound_channel("SLACK", "any"));
    }

    /// Round 2 — inbound ``decide()`` behaviour is UNCHANGED. The
    /// inbound contract still returns the registry's L2-open / L3-deny
    /// default for unconfigured platforms (L3 denies for unknown
    /// senders, but L2 is open). The outbound helper is strictly
    /// stricter on the L2 surface.
    #[test]
    fn inbound_decide_still_uses_registry_default_for_unconfigured_platform() {
        let reg = PlatformTrustConfigs::new();
        // Empty registry → inbound returns DenyIdentity (L3 deny-all
        // default) for untrusted senders. This is the pre-6.4.1D
        // behaviour and MUST NOT change.
        assert_eq!(
            reg.decide("discord", "any-channel", false, "stranger"),
            Decision::DenyIdentity
        );
        // Outbound is stricter — fails closed.
        assert!(!reg.authorize_outbound_channel("discord", "any-channel"));
    }

    // ── Phase 6.4.1E — Discord parent-channel trust inheritance ──
    //
    // The thread's own ``channel_id`` (T) is not always in the
    // operator's ``allowed_channels``; Discord threads are separate
    // channels whose parent (P) IS the surface the operator approved.
    // The canonical trust model is extended so a thread is in L2 scope
    // when ``T ∈ allowed`` OR ``P ∈ allowed``. The rule is additive —
    // every pre-6.4.1E caller that already invokes ``surface_allowed``
    // or ``decide`` continues to behave identically because the new
    // helpers are siblings, not replacements.

    /// A — parent is in the allowlist, thread is not → ALLOW via
    /// parent inheritance. DMs short-circuit on ``allow_dm``.
    #[test]
    fn surface_allowed_with_parent_inherits_from_allowed_parent() {
        let c = TrustConfig::new(
            Some(false),               // allow_all_channels closed
            ["parent-channel".into()], // only the parent is allowed
            Some(true),                // allow_dm
            None,
            std::iter::empty::<String>(),
        );
        assert!(
            c.surface_allowed_with_parent("thread-channel", Some("parent-channel"), false,),
            "A: allowed parent must allow the child thread"
        );
        // DM path is unchanged — controlled by allow_dm, parent ignored.
        assert!(c.surface_allowed_with_parent("thread-channel", Some("parent-channel"), true));
    }

    /// B — explicit ``T`` wins over parent. Even if P is not allowed,
    /// T being allowed lets the surface pass.
    #[test]
    fn surface_allowed_with_parent_explicit_channel_wins() {
        let c = TrustConfig::new(
            Some(false),
            ["thread-channel".into()], // only T is allowed
            Some(true),
            None,
            std::iter::empty::<String>(),
        );
        assert!(
            c.surface_allowed_with_parent("thread-channel", Some("other-parent"), false),
            "B: explicit T must allow even when parent is unrelated"
        );
    }

    /// C — neither T nor P in allowlist → DENY. No implicit
    /// broadening, no fallback widening.
    #[test]
    fn surface_allowed_with_parent_denies_when_neither_allowed() {
        let c = TrustConfig::new(
            Some(false),
            ["some-other-channel".into()],
            Some(true),
            None,
            std::iter::empty::<String>(),
        );
        assert!(
            !c.surface_allowed_with_parent("thread-channel", Some("parent-channel"), false),
            "C: neither T nor P in allowlist must deny"
        );
    }

    /// D — T not allowed AND no parent → DENY. Parent inheritance
    /// only applies when P is present. This preserves the pre-6.4.1E
    /// behaviour for non-thread channels and for callers that do not
    /// (yet) carry parent_id.
    #[test]
    fn surface_allowed_with_parent_denies_when_parent_missing() {
        let c = TrustConfig::new(
            Some(false),
            ["parent-channel".into()],
            Some(true),
            None,
            std::iter::empty::<String>(),
        );
        assert!(
            !c.surface_allowed_with_parent("thread-channel", None, false),
            "D: missing parent must NOT silently inherit"
        );
        // Bit-exact equivalence to the legacy helper for None parent.
        assert_eq!(
            c.surface_allowed_with_parent("thread-channel", None, false),
            c.surface_allowed("thread-channel", false),
            "D: parent_id=None must equal legacy surface_allowed"
        );
    }

    /// E — parent inheritance does NOT bypass identity. L3
    /// (``allowed_users``) still denies a stranger in an
    /// allowed-parent thread. This pins the security invariant
    /// demanded by the Phase 6.4.1E spec.
    #[test]
    fn decide_with_parent_does_not_bypass_identity_gate() {
        let c = TrustConfig::new(
            Some(false),
            ["parent-channel".into()],
            Some(true),
            None, // allow_all_users defaults to false (deny)
            ["allowed-user".into()],
        );
        // Same registry, same channel/parent, but a stranger sender.
        assert_eq!(
            c.decide_with_parent("thread-channel", Some("parent-channel"), false, "stranger"),
            Decision::DenyIdentity,
            "E: parent inheritance MUST NOT bypass allowed_users"
        );
        // Same channel/parent with the allowed user → Allow.
        assert_eq!(
            c.decide_with_parent(
                "thread-channel",
                Some("parent-channel"),
                false,
                "allowed-user"
            ),
            Decision::Allow,
            "E: allowed user in allowed-parent thread must Allow"
        );
    }

    /// Registry-level parent-aware inbound. Mirrors ``decide`` for the
    /// empty-parent case (bit-exact) and adds the parent-inheritance
    /// rule for ``Some(parent)``.
    #[test]
    fn registry_decide_with_parent_matches_decide_when_parent_missing() {
        let mut reg = PlatformTrustConfigs::new();
        reg.insert(
            "discord",
            TrustConfig::new(
                Some(false),
                ["parent-channel".into()],
                Some(true),
                None,
                ["u1".into()],
            ),
        );
        // None parent → identical to legacy decide().
        assert_eq!(
            reg.decide_with_parent("discord", "thread-channel", None, false, "u1"),
            reg.decide("discord", "thread-channel", false, "u1"),
        );
        // Some allowed parent → Allow (vs legacy which would DenyScope).
        assert_eq!(
            reg.decide_with_parent(
                "discord",
                "thread-channel",
                Some("parent-channel"),
                false,
                "u1"
            ),
            Decision::Allow,
        );
        // Some disallowed parent → DenyScope.
        assert_eq!(
            reg.decide_with_parent("discord", "thread-channel", Some("other"), false, "u1"),
            Decision::DenyScope,
        );
    }

    // ── Phase 6.4.1E — production regression ────────────────────────────
    //
    // Pin the EXACT production case so the operator can verify the
    // parent-channel inheritance is wired end-to-end without restarting
    // services:
    //
    //   allowed_channels = ["1536735741642547262"]
    //   thread:
    //     channel_id = "1544014554000789575"
    //     parent_id  = "1536735741642547262"
    //
    // Both inbound ``surface_allowed_with_parent`` and outbound
    // ``authorize_outbound_channel_with_parent`` MUST return ``true``.

    /// Production regression — inbound L2 scope passes via parent.
    #[test]
    fn production_regression_inbound_parent_inheritance_allowed() {
        let c = TrustConfig::new(
            Some(false),
            ["1536735741642547262".into()], // operator's only allowed channel
            Some(true),
            None,
            std::iter::empty::<String>(),
        );
        assert!(
            c.surface_allowed_with_parent(
                "1544014554000789575",
                Some("1536735741642547262"),
                false,
            ),
            "production regression: parent-channel inheritance must ALLOW the workflow thread"
        );
    }

    /// Production regression — outbound via the registry helper.
    #[test]
    fn production_regression_outbound_parent_inheritance_allowed() {
        let mut reg = PlatformTrustConfigs::new();
        reg.insert(
            "discord",
            TrustConfig::new(
                Some(false),
                ["1536735741642547262".into()],
                Some(true),
                None,
                std::iter::empty::<String>(),
            ),
        );
        assert!(
            reg.authorize_outbound_channel_with_parent(
                "discord",
                "1544014554000789575",
                Some("1536735741642547262"),
            ),
            "production regression: outbound structured destination must ALLOW the workflow thread"
        );
        // Sanity — without the parent, the same channel is denied.
        assert!(
            !reg.authorize_outbound_channel_with_parent("discord", "1544014554000789575", None,),
            "production regression sanity: no parent AND T not in allowlist must DENY"
        );
    }

    /// Production regression — inbound full L2+L3 path passes for an
    /// allowed user on the workflow thread.
    #[test]
    fn production_regression_inbound_decide_with_parent_allows_trusted_user() {
        let c = TrustConfig::new(
            Some(false),
            ["1536735741642547262".into()],
            Some(true),
            None,
            ["allowed-operator".into()],
        );
        assert_eq!(
            c.decide_with_parent(
                "1544014554000789575",
                Some("1536735741642547262"),
                false,
                "allowed-operator",
            ),
            Decision::Allow,
            "production regression: trusted operator in workflow thread must Allow"
        );
    }
}

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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> TrustConfig {
        // L2 open, explicit allowed channel; L3 with one allowed user.
        TrustConfig::new(
            None,                                  // allow_all_channels → true
            ["chan-1".to_string()],
            None,                                  // allow_dm → true
            None,                                  // allow_all_users → false (deny)
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
        assert_eq!(cfg().decide("any-channel", false, "user-1"), Decision::Allow);
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
        assert_eq!(cfg().decide("dm-chan", true, "stranger"), Decision::DenyIdentity);
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
        let c = TrustConfig::new(None, std::iter::empty(), None, Some(true), std::iter::empty());
        assert_eq!(c.decide("c", false, "anyone"), Decision::Allow);
    }

    #[test]
    fn dm_closed_denies_scope_even_for_allowed_user() {
        let c = TrustConfig::new(None, std::iter::empty(), Some(false), None, ["user-1".to_string()]);
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
        assert_eq!(reg.decide("mars", "c", false, "stranger"), Decision::DenyIdentity);
    }

    #[test]
    fn registry_uses_registered_platform_config() {
        let mut reg = PlatformTrustConfigs::new();
        reg.insert(
            "telegram",
            TrustConfig::new(None, std::iter::empty(), None, None, ["123".to_string()]),
        );
        assert_eq!(reg.decide("telegram", "c", false, "123"), Decision::Allow);
        assert_eq!(reg.decide("telegram", "c", false, "999"), Decision::DenyIdentity);
        // unregistered platform still gets deny-all default.
        assert_eq!(reg.decide("discord", "c", false, "123"), Decision::DenyIdentity);
    }

    #[test]
    fn empty_sender_is_never_identity_allowed() {
        // Even with allow_all_users = true, an empty sender_id fails closed.
        let open = TrustConfig::new(None, std::iter::empty(), None, Some(true), std::iter::empty());
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
}

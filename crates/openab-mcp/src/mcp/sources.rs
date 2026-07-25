//! Session-aware in-process capability sources for the OAB MCP Facade.
//!
//! The facade's catalog historically had one origin: downstream MCP servers
//! from `mcp.json` (host-level — every connected client sees the same
//! catalog). Some capabilities are **session-bound** instead: they must be
//! routed to the chat session that owns them (e.g. browser control, where
//! `browser.click` must reach *that conversation's* browser tab, #1447).
//!
//! This module adds the second origin:
//!
//! - [`CapabilitySource`] — an in-process provider registered at facade
//!   construction (no `mcp.json` entry, no extra listener, no subprocess).
//!   Sources receive an optional [`SessionCtx`] on every call.
//! - [`SessionTokens`] — the broker↔facade contract for identity: the broker
//!   mints one opaque token per agent session (written into that agent's MCP
//!   client config as an `Authorization: Bearer` header) and revokes it on
//!   session evict. The facade resolves the header back to a [`SessionCtx`]
//!   per request via the HTTP parts rmcp injects into request extensions.
//!
//! Anonymous clients (no/unknown token) keep working unchanged: they see the
//! host-level catalog plus any sources with `requires_session() == false`.
//! Session-bound sources are invisible to them — discovery and execution
//! both gate on a resolved context, so there is no "visible but always
//! fails" surface.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::Result;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64_URL;
use base64::Engine as _;
use rmcp::model::Tool;
use serde_json::{Map, Value};

/// Identity of the downstream agent session a facade request belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCtx {
    /// The chat-session/channel id the broker keyed this session by.
    pub channel_id: String,
}

/// An in-process capability provider behind the facade.
///
/// Implementations live wherever their backing state lives (the root binary
/// for tunnel-backed sources, adapter crates for API-backed ones) and are
/// registered via [`super::facade::serve_http_with`]. Registration is the
/// operator's grant: sources are code-wired by the broker, so unlike
/// `mcp.json` servers there is no per-source `tool_filter` — do not register
/// a source whose full tool set you don't intend to expose.
#[async_trait::async_trait]
pub trait CapabilitySource: Send + Sync {
    /// Provider label surfaced in discovery entries and audit lines.
    fn provider(&self) -> &str;

    /// The advertised tool set. `ctx` is `None` for anonymous clients.
    /// Sources may vary the set by session, but static-advertising
    /// regardless of backend attachment (D4, #1447) is the recommended
    /// default — availability problems belong in call errors, not in
    /// catalog flapping.
    fn tools(&self, ctx: Option<&SessionCtx>) -> Vec<Tool>;

    /// Execute one tool. Returns `(payload, is_error)` mirroring the MCP
    /// `CallToolResult` split the meta-tool dispatcher uses.
    async fn call(
        &self,
        ctx: Option<&SessionCtx>,
        tool: &str,
        args: &Map<String, Value>,
    ) -> Result<(Value, bool)>;

    /// Session-bound sources return `true`: anonymous clients neither see
    /// their tools in discovery nor can execute them.
    fn requires_session(&self) -> bool {
        false
    }
}

/// Broker↔facade session-token registry. Cheap to clone (shared inner map);
/// the broker holds one side (mint/revoke on session lifecycle), the facade
/// the other (resolve per request).
#[derive(Clone, Default)]
pub struct SessionTokens {
    inner: Arc<RwLock<HashMap<String, SessionCtx>>>,
}

impl SessionTokens {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a fresh opaque token bound to `channel_id`. A prior token for
    /// the same channel (e.g. a respawned session) is replaced — exactly one
    /// live token per channel.
    pub fn mint(&self, channel_id: &str) -> String {
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf).expect("os rng");
        let token = B64_URL.encode(buf);
        let mut map = self.inner.write().expect("session token lock");
        map.retain(|_, ctx| ctx.channel_id != channel_id);
        map.insert(
            token.clone(),
            SessionCtx {
                channel_id: channel_id.to_string(),
            },
        );
        token
    }

    /// Revoke every token for `channel_id` (session evict / respawn).
    pub fn revoke_channel(&self, channel_id: &str) {
        self.inner
            .write()
            .expect("session token lock")
            .retain(|_, ctx| ctx.channel_id != channel_id);
    }

    /// Resolve a presented token. Constant-time comparison over stored
    /// tokens so a colocated process can't probe a token byte-by-byte via
    /// response timing (session counts are small; the linear scan is noise).
    pub fn resolve(&self, presented: &str) -> Option<SessionCtx> {
        let map = self.inner.read().expect("session token lock");
        let mut found: Option<SessionCtx> = None;
        for (token, ctx) in map.iter() {
            let eq: bool = constant_time_eq(token.as_bytes(), presented.as_bytes());
            if eq && found.is_none() {
                found = Some(ctx.clone());
            }
        }
        found
    }
}

/// Constant-time byte comparison (length leak is fine — token length is
/// public). No `subtle` dependency in this crate; the loop below is the
/// textbook fold that optimizers are documented not to short-circuit when
/// the accumulator is observed.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut acc: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

/// Resolve a [`SessionCtx`] from the HTTP parts rmcp injects into request
/// extensions (`http::request::Parts`, see rmcp streamable-http server
/// docs): `Authorization: Bearer <token>` → token registry lookup. Absent
/// parts (non-HTTP transports), absent/malformed header, or an unknown
/// token all resolve to `None` — the anonymous, host-level view.
pub fn session_ctx_from_extensions(
    extensions: &rmcp::model::Extensions,
    tokens: &SessionTokens,
) -> Option<SessionCtx> {
    let parts = extensions.get::<axum::http::request::Parts>()?;
    let bearer = parts
        .headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    tokens.resolve(bearer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_resolve_revoke_lifecycle() {
        let tokens = SessionTokens::new();
        let t1 = tokens.mint("chan-a");
        assert_eq!(tokens.resolve(&t1).unwrap().channel_id, "chan-a");
        assert!(tokens.resolve("nope").is_none());
        // Re-mint for the same channel replaces the old token.
        let t2 = tokens.mint("chan-a");
        assert!(tokens.resolve(&t1).is_none(), "old token must be dead");
        assert_eq!(tokens.resolve(&t2).unwrap().channel_id, "chan-a");
        tokens.revoke_channel("chan-a");
        assert!(tokens.resolve(&t2).is_none());
    }

    #[test]
    fn constant_time_eq_basics() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn ctx_resolution_from_http_parts() {
        let tokens = SessionTokens::new();
        let tok = tokens.mint("chan-b");
        let make_ext = |auth: Option<String>| {
            let mut b = axum::http::Request::builder().uri("/mcp");
            if let Some(a) = auth {
                b = b.header(axum::http::header::AUTHORIZATION, a);
            }
            let (parts, ()) = b.body(()).unwrap().into_parts();
            let mut ext = rmcp::model::Extensions::new();
            ext.insert(parts);
            ext
        };
        let ctx = session_ctx_from_extensions(&make_ext(Some(format!("Bearer {tok}"))), &tokens);
        assert_eq!(ctx.unwrap().channel_id, "chan-b");
        assert!(
            session_ctx_from_extensions(&make_ext(Some("Bearer wrong".into())), &tokens).is_none()
        );
        assert!(session_ctx_from_extensions(&make_ext(None), &tokens).is_none());
        // No http parts at all (e.g. non-HTTP transport) → anonymous.
        assert!(session_ctx_from_extensions(&rmcp::model::Extensions::new(), &tokens).is_none());
    }
}

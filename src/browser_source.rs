//! Browser capability source (Facade mode): serves the browser tool set as a
//! **session-aware in-process capability source** of the OAB MCP Facade
//! (`openab_mcp::mcp::sources`), replacing the per-session loopback proxy as
//! the default transport. Identity comes from the broker-minted session
//! token (`OPENAB_SESSION_TOKEN` in the agent's env → `Authorization` header
//! → `SessionCtx`), and calls route into the same MCP-over-ACP tunnel the
//! proxy used — `channel_id` semantics unchanged.
//!
//! Root-hosted because it needs both worlds: `openab_mcp`'s source trait and
//! `openab_core`'s tunnel bridge (core and the mcp crate stay independent).

use std::sync::Arc;

use anyhow::{anyhow, Result};
use openab_core::mcp_proxy::AcpMcpTunnel;
use openab_mcp::mcp::sources::{CapabilitySource, SessionCtx};
use serde_json::{json, Map, Value};

/// Facade capability source backed by the browser MCP-over-ACP tunnel.
pub struct BrowserSource {
    tunnel: Arc<dyn AcpMcpTunnel>,
}

impl BrowserSource {
    pub fn new(tunnel: Arc<dyn AcpMcpTunnel>) -> Self {
        Self { tunnel }
    }
}

#[async_trait::async_trait]
impl CapabilitySource for BrowserSource {
    fn provider(&self) -> &str {
        "openab-browser"
    }

    /// D4 static-advertise (unchanged from proxy mode): the tool set is
    /// constant regardless of extension attachment — a call while
    /// disconnected returns a "browser not connected" error result rather
    /// than catalog flapping.
    fn tools(&self, _ctx: Option<&SessionCtx>) -> Vec<openab_mcp::rmcp::model::Tool> {
        openab_core::mcp_proxy::browser_tools()
    }

    async fn call(
        &self,
        ctx: Option<&SessionCtx>,
        tool: &str,
        args: &Map<String, Value>,
    ) -> Result<(Value, bool)> {
        // requires_session() guarantees ctx in practice; defend anyway.
        let ctx = ctx.ok_or_else(|| anyhow!("browser capabilities require a session token"))?;
        let params = json!({ "name": tool, "arguments": args });
        // Empty server_id sentinel (Fork A) — same routing contract as the
        // per-session proxy: RootBrowserTunnel resolves the sole tunnel on
        // the channel.
        match self
            .tunnel
            .call(&ctx.channel_id, "", "tools/call", Some(params))
            .await
        {
            // The tunnel returns the inner MCP CallToolResult payload; pass
            // it through and mirror its own isError flag.
            Ok(result) => {
                let is_error = result
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok((result, is_error))
            }
            // Tunnel-level failure (no extension attached, session gone):
            // an error *result* — the agent gets an actionable message, the
            // facade dispatch itself did not fault.
            Err(msg) => Ok((
                json!({ "content": [{ "type": "text", "text": msg }], "isError": true }),
                true,
            )),
        }
    }

    fn requires_session(&self) -> bool {
        true
    }
}

/// Root-side adapter: exposes the facade's `SessionTokens` registry through
/// core's `SessionTokenRegistrar` hook (core cannot depend on openab-mcp).
pub struct FacadeRegistrar(pub openab_mcp::mcp::sources::SessionTokens);

impl openab_core::mcp_proxy::SessionTokenRegistrar for FacadeRegistrar {
    fn mint(&self, channel_id: &str) -> String {
        self.0.mint(channel_id)
    }
    fn revoke(&self, channel_id: &str) {
        self.0.revoke_channel(channel_id)
    }
}

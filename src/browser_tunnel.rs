//! Root-side bridge implementing the core `BrowserTunnel` trait (D6-a'). Reads the gateway's
//! per-session MCP-over-ACP tunnel registry and forwards a tool call to the browser attached
//! to a given `channel_id`. This lives in the root binary — the only place that depends on
//! BOTH openab-core (the trait) and openab-gateway (the `TunnelHandle`), preserving the two
//! crates' sibling independence (mirroring the existing `ChatAdapter` glue at the root).

use openab_core::mcp_proxy::BrowserTunnel;
use openab_gateway::adapters::acp_server::AcpTunnelRegistry;
use serde_json::Value;

pub struct RootBrowserTunnel {
    registry: AcpTunnelRegistry,
}

impl RootBrowserTunnel {
    pub fn new(registry: AcpTunnelRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl BrowserTunnel for RootBrowserTunnel {
    async fn call(
        &self,
        channel_id: &str,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, String> {
        // Clone the handle out under the lock; never hold the std mutex across `.await`.
        let handle = {
            let reg = self.registry.lock().unwrap_or_else(|e| e.into_inner());
            reg.get(channel_id).cloned()
        };
        match handle {
            Some(h) => h.mcp_message(method, params, 30).await,
            None => Err(format!("no browser attached to session {channel_id}")),
        }
    }
}

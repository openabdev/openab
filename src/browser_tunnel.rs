//! Root-side bridge implementing the core `AcpMcpTunnel` trait (D6-a'). Reads the gateway's
//! per-session MCP-over-ACP tunnel registry and forwards a tool call to the client MCP server
//! attached to a given `(channel_id, server_id)`. This lives in the root binary — the only place
//! that depends on BOTH openab-core (the trait) and openab-gateway (the `TunnelHandle`),
//! preserving the two crates' sibling independence (mirroring the existing `ChatAdapter` glue at
//! the root).

use openab_core::mcp_proxy::AcpMcpTunnel;
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
impl AcpMcpTunnel for RootBrowserTunnel {
    async fn call(
        &self,
        channel_id: &str,
        server_id: &str,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, String> {
        // Clone the handle out under the lock; never hold the std mutex across `.await`.
        let handle = {
            let reg = self.registry.lock().unwrap_or_else(|e| e.into_inner());
            if server_id.is_empty() {
                // Fork A sentinel: the single-browser proxy/bridge doesn't know the client-
                // declared server id, so resolve the SOLE tunnel registered for this channel.
                // Ambiguous (0 or >1) is an error — real per-server routing arrives in P2.
                let mut matches = reg.iter().filter(|((c, _), _)| c == channel_id);
                match (matches.next(), matches.next()) {
                    (Some((_, h)), None) => Some(h.clone()),
                    (None, _) => None,
                    (Some(_), Some(_)) => {
                        return Err(format!(
                            "multiple MCP servers attached to session {channel_id}; a server_id is required to disambiguate"
                        ));
                    }
                }
            } else {
                reg.get(&(channel_id.to_string(), server_id.to_string()))
                    .cloned()
            }
        };
        match handle {
            Some(h) => h.mcp_message(method, params, 30).await,
            None => Err(format!("no browser attached to session {channel_id}")),
        }
    }
}

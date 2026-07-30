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
    /// Operator-configurable ceiling for one tunnelled request (`[mcp] tunnel_timeout_seconds`).
    /// Carried here rather than read per call so the value a session runs under is fixed when the
    /// tunnel is wired, not re-resolved mid-flight.
    timeout_secs: u64,
}

impl RootBrowserTunnel {
    pub fn new(registry: AcpTunnelRegistry, timeout_secs: u64) -> Self {
        Self {
            registry,
            timeout_secs,
        }
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
            Some(h) => h.mcp_message(method, params, self.timeout_secs).await,
            // Redacted here, at construction. This string is RETURNED, not logged — it reaches a
            // caller that may log it, surface it to a model, or put it in an error response, so no
            // logging-side redaction point can ever catch it. An id embedded mid-sentence also
            // escapes every field-name scan: the pattern that found the other sites cannot see it.
            None => Err(format!(
                "no browser attached to session {}",
                openab_core::redact::redact_session_ids(channel_id)
            )),
        }
    }

    /// Delegates to the gateway, which resolves under the registry lock and beside the eviction
    /// that keeps declared names unique. Deliberately not implemented here by enumerating and
    /// matching: this crate cannot rank two tunnels — the ordering fields are private to the gateway
    /// — so a local implementation could only take an arbitrary match or refuse, and refusing is the
    /// behaviour ADR §6.1 rejects.
    fn resolve_by_name(&self, channel_id: &str, server_name: &str) -> Option<String> {
        openab_gateway::adapters::acp_server::resolve_by_name(
            &self.registry,
            channel_id,
            server_name,
        )
    }

}

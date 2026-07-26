//! ACP-tunnel capability source (Facade mode): serves **client-declared** MCP
//! servers as a **session-aware in-process capability source** of the OAB MCP
//! Facade (`openab_mcp::mcp::sources`), replacing the per-session loopback
//! proxy as the default transport. Identity comes from the broker-minted
//! session token (`OPENAB_SESSION_TOKEN` in the agent's env → `Authorization`
//! header → `SessionCtx`), and calls route into the MCP-over-ACP tunnel the
//! proxy used — `channel_id` semantics unchanged.
//!
//! Root-hosted because it needs both worlds: `openab_mcp`'s source trait and
//! `openab_core`'s tunnel bridge (core and the mcp crate stay independent).
//!
//! **One source, N servers** (ADR §6.2). Facade sources are registered once at
//! construction, so there is no source-per-declared-server; this one fans out
//! internally, routing the `<server>.<tool>` prefix to the right tunnel.
//!
//! **The prefix is a declared `name`, not a registry key** (ADR §6.1). A
//! declaration is `{type:"acp", id, name}`; the reference client mints `id` as
//! a fresh UUID per connection while `name` (`"browser"`) is stable. Routing
//! therefore resolves `name` → `(channel_id, id)` through the tunnel
//! registry's enumeration, and forwards the **full** published tool name
//! (`browser.click`) — the prefix selects the tunnel, it is not stripped,
//! because the server's own `tools/call` expects its full name.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use openab_core::mcp_proxy::AcpMcpTunnel;
use openab_mcp::mcp::sources::{CapabilitySource, SessionCtx};
use openab_mcp::rmcp::model::Tool;
use serde_json::{json, Map, Value};

/// Trust policy for one declared server name (ADR §6.4).
///
/// #1454 treats source registration as the operator's grant, so facade sources
/// carry no `tool_filter`. That holds for code-wired sources whose tool set the
/// operator chose; it does **not** hold here, where the tool set is declared by
/// a remote client. The declared *name* is chosen by that same client, so
/// passing the allowlist grants nothing by itself — the tool set is gated
/// separately, and **deny-all is the default**: a name with no policy entry is
/// refused outright, and a listed name may only publish the tools pinned here.
#[derive(Clone)]
struct ServerPolicy {
    /// Exactly the tool names this server may publish. Anything else it declares
    /// is dropped from the catalog and refused on call, so a client cannot
    /// inject new tools by re-declaring a trusted name.
    allowed: HashSet<String>,
    /// **Pre-attach seed** (ADR §6.3): full `Tool` values advertised from the
    /// moment the source is registered, so a permitted server is discoverable
    /// before its client ever attaches — the property D4 relies on. Known
    /// servers seed from the built-in catalog; a server an operator admitted by
    /// name alone seeds empty until discovery caching supplies real schemas.
    /// Always a subset of `allowed`: the seed narrows with the policy, never
    /// past it.
    seed: Vec<Tool>,
}

/// Built-in tool catalogs, by declared server name. The only source of full
/// `Tool` values (schemas included) before a server has been queried.
///
/// Browser-ness lives here as **policy data**, deliberately not as a branch in
/// the routing code — that is what keeps the source generic (ADR §6.2: "the
/// source must contain no browser-specific branch").
fn builtin_catalogs() -> HashMap<String, Vec<Tool>> {
    HashMap::from([(
        "browser".to_string(),
        openab_core::mcp_proxy::browser_tools(),
    )])
}

/// The default policy when an operator has configured nothing: `browser` pinned
/// to its five known tools, every other declared name denied.
fn default_policy() -> HashMap<String, ServerPolicy> {
    builtin_catalogs()
        .into_iter()
        .map(|(name, tools)| {
            let policy = ServerPolicy {
                allowed: tools.iter().map(|t| t.name.to_string()).collect(),
                seed: tools,
            };
            (name, policy)
        })
        .collect()
}

/// Build the policy from the operator's `[[mcp.acp_servers]]` entries (§6.4).
///
/// Each entry admits one declared **name** and lists exactly the tools it may
/// publish; deny-all means an entry with no tools admits the server but grants
/// nothing. Names are enough — schemas are taken from the built-in catalog
/// where one exists, narrowed to what the operator permitted, so an operator
/// can *restrict* a known server without restating its schemas. A permitted
/// tool with no known schema simply has no seed yet and appears once discovery
/// caching can fetch it.
fn policy_from_config(entries: &[openab_core::config::AcpServerPolicy]) -> HashMap<String, ServerPolicy> {
    let mut catalogs = builtin_catalogs();
    entries
        .iter()
        .map(|entry| {
            let allowed: HashSet<String> = entry.tools.iter().cloned().collect();
            let seed = catalogs
                .remove(&entry.name)
                .unwrap_or_default()
                .into_iter()
                .filter(|t| allowed.contains(t.name.as_ref()))
                .collect();
            (entry.name.clone(), ServerPolicy { allowed, seed })
        })
        .collect()
}

/// Facade capability source backed by MCP-over-ACP tunnels to client-declared
/// MCP servers.
pub struct AcpTunnelSource {
    tunnel: Arc<dyn AcpMcpTunnel>,
    /// Trust policy keyed by declared server **name** (§6.4). Keyed by name
    /// rather than id because ids are per-connection UUIDs — an allowlist of
    /// ids could never match twice.
    policy: HashMap<String, ServerPolicy>,
}

impl AcpTunnelSource {
    /// Source gated by the operator's `[[mcp.acp_servers]]` entries. An empty
    /// list keeps the built-in default rather than denying everything, so
    /// omitting the section leaves browser control working as before; an
    /// operator who writes any entry takes over the allowlist wholesale.
    pub fn with_config(
        tunnel: Arc<dyn AcpMcpTunnel>,
        entries: &[openab_core::config::AcpServerPolicy],
    ) -> Self {
        let policy = if entries.is_empty() {
            default_policy()
        } else {
            policy_from_config(entries)
        };
        Self { tunnel, policy }
    }

    /// Split a published tool name into `(server_name, full_tool_name)`. The
    /// full name is returned deliberately: the prefix picks the tunnel, and the
    /// server's own `tools/call` expects the name it published.
    fn split_prefix(tool: &str) -> Option<(&str, &str)> {
        let (server, _rest) = tool.split_once('.')?;
        if server.is_empty() {
            return None;
        }
        Some((server, tool))
    }

    /// An error *result* (not a dispatch fault): the agent gets an actionable
    /// message and the facade's own dispatch is considered to have succeeded —
    /// matching how tunnel unavailability is reported.
    fn error_result(message: String) -> (Value, bool) {
        (
            json!({ "content": [{ "type": "text", "text": message }], "isError": true }),
            true,
        )
    }
}

#[async_trait::async_trait]
impl CapabilitySource for AcpTunnelSource {
    fn provider(&self) -> &str {
        "openab-browser"
    }

    /// The advertised catalog: every tool the trust policy pins, for every
    /// allowlisted server.
    ///
    /// Deliberately **not** intersected with the tunnels currently attached.
    /// Attachment flapping must not reach the catalog (§6.3) — a tab that is
    /// closed for a second must not make the tools vanish and reappear — so
    /// availability is reported by `call` ("browser not connected"), never by a
    /// shrinking tool list. This keeps the pre-attach discovery behaviour the
    /// static-advertise design (D4) already had.
    ///
    /// Session *scope* — restricting the catalog to the servers this client
    /// actually declared — is a different axis and needs the per-`(channel_id,
    /// server_id)` declaration cache that F3′ introduces; until then an
    /// allowlisted server's pinned tools are advertised to every session, which
    /// is exactly the status quo for the browser.
    fn tools(&self, _ctx: Option<&SessionCtx>) -> Vec<Tool> {
        let mut out: Vec<Tool> = self
            .policy
            .values()
            .flat_map(|p| p.seed.iter().cloned())
            .collect();
        // Stable order: the catalog is user-visible and a HashMap iteration
        // order would reshuffle it between runs.
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    async fn call(
        &self,
        ctx: Option<&SessionCtx>,
        tool: &str,
        args: &Map<String, Value>,
    ) -> Result<(Value, bool)> {
        // requires_session() guarantees ctx in practice; defend anyway.
        let ctx = ctx.ok_or_else(|| anyhow!("ACP tunnel capabilities require a session token"))?;

        let Some((server_name, full_tool)) = Self::split_prefix(tool) else {
            return Ok(Self::error_result(format!(
                "malformed tool name {tool:?}: expected <server>.<tool>"
            )));
        };

        // §6.4 gate, in two independent steps: the name must be allowlisted,
        // and the tool must be one this server is pinned to. The second check
        // is what stops a client injecting tools by re-declaring a trusted
        // name, so it is not redundant with the first.
        let Some(policy) = self.policy.get(server_name) else {
            return Ok(Self::error_result(format!(
                "server {server_name:?} is not in the operator allowlist"
            )));
        };
        if !policy.allowed.contains(full_tool) {
            return Ok(Self::error_result(format!(
                "tool {full_tool:?} is not permitted for server {server_name:?}"
            )));
        }

        // Resolve the declared name to the tunnel's registry key (§6.1). Same-name
        // duplicates cannot occur — attach evicts the stale entry (last-attach-wins).
        let Some((_, server_id)) = self
            .tunnel
            .servers(&ctx.channel_id)
            .into_iter()
            .find(|(name, _)| name == server_name)
        else {
            return Ok(Self::error_result(format!(
                "{server_name} not connected: open the OpenAB side panel in your browser"
            )));
        };

        let params = json!({ "name": full_tool, "arguments": args });
        match self
            .tunnel
            .call(&ctx.channel_id, &server_id, "tools/call", Some(params))
            .await
        {
            // The tunnel returns the inner MCP CallToolResult payload; pass it
            // through and mirror its own isError flag.
            Ok(result) => {
                let is_error = result
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok((result, is_error))
            }
            // Tunnel-level failure (extension detached mid-call, session gone):
            // an error result, not a dispatch fault.
            Err(msg) => Ok(Self::error_result(msg)),
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

#[cfg(test)]
mod tests {
    use super::{AcpTunnelSource, CapabilitySource, SessionCtx};
    use openab_core::mcp_proxy::AcpMcpTunnel;
    use serde_json::{json, Map, Value};
    use std::sync::Arc;

    /// Tunnel double: reports one declared server and records what was forwarded.
    struct FakeTunnel {
        servers: Vec<(String, String)>,
        forwarded: std::sync::Mutex<Vec<(String, String, Value)>>,
    }

    impl FakeTunnel {
        fn with(servers: &[(&str, &str)]) -> Arc<Self> {
            Arc::new(Self {
                servers: servers
                    .iter()
                    .map(|(n, i)| (n.to_string(), i.to_string()))
                    .collect(),
                forwarded: std::sync::Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait::async_trait]
    impl AcpMcpTunnel for FakeTunnel {
        async fn call(
            &self,
            channel_id: &str,
            server_id: &str,
            _method: &str,
            params: Option<Value>,
        ) -> Result<Value, String> {
            self.forwarded.lock().unwrap().push((
                channel_id.to_string(),
                server_id.to_string(),
                params.unwrap_or(Value::Null),
            ));
            Ok(json!({ "content": [{ "type": "text", "text": "ok" }] }))
        }

        fn servers(&self, _channel_id: &str) -> Vec<(String, String)> {
            self.servers.clone()
        }
    }

    fn ctx() -> SessionCtx {
        SessionCtx {
            channel_id: "acp_x".into(),
        }
    }

    #[test]
    fn catalog_is_the_pinned_policy_set_and_survives_detachment() {
        // No tunnels attached at all: the catalog must NOT shrink (§6.3 — attachment
        // flapping stays out of discovery; unavailability is reported on call).
        let src = AcpTunnelSource::with_config(FakeTunnel::with(&[]), &[]);
        let names: Vec<String> = src.tools(None).iter().map(|t| t.name.to_string()).collect();
        assert_eq!(
            names,
            [
                "browser.click",
                "browser.navigate",
                "browser.read_dom",
                "browser.screenshot",
                "browser.type"
            ],
            "the pinned browser set is advertised regardless of attach state"
        );
    }

    #[tokio::test]
    async fn call_routes_the_name_prefix_to_the_declared_id_keeping_the_full_tool_name() {
        let tunnel = FakeTunnel::with(&[("browser", "uuid-abc")]);
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[]);
        let (_v, is_err) = src
            .call(Some(&ctx()), "browser.click", &Map::new())
            .await
            .unwrap();
        assert!(!is_err);

        let fwd = tunnel.forwarded.lock().unwrap();
        let (channel, server_id, params) = &fwd[0];
        assert_eq!(channel, "acp_x");
        assert_eq!(
            server_id, "uuid-abc",
            "the declared NAME must resolve to the registry id, not be used as the key"
        );
        assert_eq!(
            params["name"], "browser.click",
            "the prefix selects the tunnel and is NOT stripped — the server published this name"
        );
    }

    #[tokio::test]
    async fn unlisted_server_name_is_refused_even_when_a_tunnel_is_attached() {
        // A client declaring an un-allowlisted name must not reach the agent's catalog
        // OR its dispatch, even though its tunnel is registered.
        let tunnel = FakeTunnel::with(&[("evil", "uuid-evil")]);
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[]);
        let (v, is_err) = src
            .call(Some(&ctx()), "evil.exfiltrate", &Map::new())
            .await
            .unwrap();
        assert!(is_err);
        assert!(v["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("not in the operator allowlist"));
        assert!(
            tunnel.forwarded.lock().unwrap().is_empty(),
            "a denied call must never reach the tunnel"
        );
    }

    #[tokio::test]
    async fn unpinned_tool_on_an_allowlisted_server_is_refused() {
        // The injection Falcon flagged: the client re-declares the trusted name
        // `browser` but publishes a tool outside its pinned five.
        let tunnel = FakeTunnel::with(&[("browser", "uuid-abc")]);
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[]);
        let (v, is_err) = src
            .call(Some(&ctx()), "browser.exec", &Map::new())
            .await
            .unwrap();
        assert!(is_err);
        assert!(v["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("is not permitted"));
        assert!(
            tunnel.forwarded.lock().unwrap().is_empty(),
            "an unpinned tool must never reach the tunnel"
        );
    }

    #[tokio::test]
    async fn allowlisted_but_unattached_server_reports_not_connected() {
        let tunnel = FakeTunnel::with(&[]);
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[]);
        let (v, is_err) = src
            .call(Some(&ctx()), "browser.click", &Map::new())
            .await
            .unwrap();
        assert!(is_err);
        assert!(v["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("not connected"));
    }

    fn cfg(name: &str, tools: &[&str]) -> openab_core::config::AcpServerPolicy {
        // Deserialized rather than struct-literalled so the test also pins the
        // TOML shape operators actually write.
        serde_json::from_value(json!({
            "name": name,
            "tools": tools,
        }))
        .unwrap()
    }

    #[test]
    fn absent_config_keeps_the_builtin_browser_default() {
        // Omitting [[mcp.acp_servers]] must not deny everything — existing
        // browser control keeps working untouched.
        let src = AcpTunnelSource::with_config(FakeTunnel::with(&[]), &[]);
        assert_eq!(src.tools(None).len(), 5);
    }

    #[test]
    fn operator_can_narrow_a_builtin_server_without_restating_schemas() {
        let src = AcpTunnelSource::with_config(
            FakeTunnel::with(&[("browser", "uuid-abc")]),
            &[cfg("browser", &["browser.read_dom"])],
        );
        let names: Vec<String> = src.tools(None).iter().map(|t| t.name.to_string()).collect();
        assert_eq!(
            names,
            ["browser.read_dom"],
            "the seed narrows to the operator's list, keeping the built-in schema"
        );
        assert!(
            src.tools(None)[0].input_schema.get("type").is_some(),
            "narrowing must not lose the built-in schema"
        );
    }

    #[tokio::test]
    async fn narrowing_the_policy_actually_refuses_the_dropped_tool() {
        let tunnel = FakeTunnel::with(&[("browser", "uuid-abc")]);
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[cfg("browser", &["browser.read_dom"])]);
        let (_v, is_err) = src
            .call(Some(&ctx()), "browser.click", &Map::new())
            .await
            .unwrap();
        assert!(is_err, "a tool the operator removed must be refused");
        assert!(tunnel.forwarded.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn operator_may_admit_an_unknown_server_by_name_alone() {
        // No built-in catalog for "notes": it contributes no seed yet (schemas
        // arrive with discovery caching), but its listed tool is permitted and
        // routes to the declared id.
        let tunnel = FakeTunnel::with(&[("notes", "uuid-n")]);
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[cfg("notes", &["notes.list"])]);
        assert!(
            src.tools(None).is_empty(),
            "a name-only server has no schemas to advertise until they are fetched"
        );

        let (_v, is_err) = src
            .call(Some(&ctx()), "notes.list", &Map::new())
            .await
            .unwrap();
        assert!(!is_err, "a permitted tool must dispatch even with no seed");
        assert_eq!(tunnel.forwarded.lock().unwrap()[0].1, "uuid-n");
    }

    #[tokio::test]
    async fn an_entry_with_no_tools_admits_the_server_but_grants_nothing() {
        let tunnel = FakeTunnel::with(&[("browser", "uuid-abc")]);
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[cfg("browser", &[])]);
        assert!(src.tools(None).is_empty(), "deny-all: no tools listed");
        let (_v, is_err) = src
            .call(Some(&ctx()), "browser.click", &Map::new())
            .await
            .unwrap();
        assert!(is_err);
        assert!(tunnel.forwarded.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn configuring_other_servers_drops_the_browser_default() {
        // Writing any entry takes over the allowlist wholesale — browser is not
        // silently retained alongside the operator's list.
        let tunnel = FakeTunnel::with(&[("browser", "uuid-abc")]);
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[cfg("notes", &["notes.list"])]);
        let (_v, is_err) = src
            .call(Some(&ctx()), "browser.click", &Map::new())
            .await
            .unwrap();
        assert!(is_err, "browser is no longer allowlisted once config is written");
        assert!(tunnel.forwarded.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn malformed_tool_name_without_a_prefix_is_rejected() {
        let src = AcpTunnelSource::with_config(FakeTunnel::with(&[("browser", "uuid-abc")]), &[]);
        let (v, is_err) = src.call(Some(&ctx()), "click", &Map::new()).await.unwrap();
        assert!(is_err);
        assert!(v["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("expected <server>.<tool>"));
    }
}

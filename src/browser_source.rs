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
//! a fresh UUID per connection while `name` (`"katashiro"`) is stable. Routing
//! therefore resolves `name` → `(channel_id, id)` through the tunnel
//! registry's enumeration, and forwards the **full** published tool name
//! (`katashiro.click`) — the prefix selects the tunnel, it is not stripped,
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
        "katashiro".to_string(),
        openab_core::mcp_proxy::browser_tools(),
    )])
}

/// The default policy when an operator has configured nothing: `katashiro` pinned
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
    /// Discovered tool sets, keyed `(channel_id, declared_name)` (§6.3).
    ///
    /// Keyed by **name**, not by `server_id` as an earlier draft of §6.3 said:
    /// the client mints a fresh id per connection, so an id-keyed entry would be
    /// orphaned by the very reconnect the cache exists to paper over. Keying by
    /// name is what lets a discovered set survive a reconnect, which is the
    /// cache's entire purpose.
    ///
    /// Holds what the server published; the policy filter is applied on read, so
    /// tightening the policy takes effect immediately without invalidating it.
    cache: ToolsCache,
    /// Discovery fetches currently in flight, so repeated discovery rounds do
    /// not pile up duplicate `tools/list` requests on one tunnel.
    inflight: InflightKeys,
}

/// Tool sets discovered from client servers, keyed `(channel_id, declared_name)`.
type ToolsCache = Arc<std::sync::Mutex<HashMap<(String, String), Vec<Tool>>>>;

/// `(channel_id, declared_name)` pairs with a discovery fetch already running.
type InflightKeys = Arc<std::sync::Mutex<HashSet<(String, String)>>>;

/// Sort a tool set by name: the catalog is user-visible and `HashMap` iteration
/// order would otherwise reshuffle it between runs.
fn sorted(tools: impl IntoIterator<Item = Tool>) -> Vec<Tool> {
    let mut out: Vec<Tool> = tools.into_iter().collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
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
        Self {
            tunnel,
            policy,
            cache: Arc::new(std::sync::Mutex::new(HashMap::new())),
            inflight: Arc::new(std::sync::Mutex::new(HashSet::new())),
        }
    }

    /// Fetch one server's real `tools/list` in the background and cache it
    /// (§6.3). Cheap to call on every discovery round: it is a no-op while a
    /// fetch for the same `(channel, name)` is already in flight.
    ///
    /// What lands in the cache is what the server published — **unfiltered**.
    /// The policy is applied when the catalog is read, so caching is never
    /// itself a grant: an entry for a tool the operator did not permit stays
    /// invisible and uncallable.
    fn spawn_discovery(&self, channel_id: &str, name: &str, server_id: &str) {
        // Outside a tokio runtime (unit tests calling tools() directly) there is
        // nothing to spawn onto; discovery is best-effort, so skip quietly.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let key = (channel_id.to_string(), name.to_string());
        {
            let mut inflight = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            if !inflight.insert(key.clone()) {
                return;
            }
        }
        let tunnel = self.tunnel.clone();
        let cache = self.cache.clone();
        let inflight = self.inflight.clone();
        let server_id = server_id.to_string();
        let channel = key.0.clone();
        handle.spawn(async move {
            let fetched = tunnel
                .call(&channel, &server_id, "tools/list", None)
                .await
                .ok()
                .and_then(|v| serde_json::from_value::<Vec<Tool>>(v.get("tools")?.clone()).ok());
            if let Some(tools) = fetched {
                cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(key.clone(), tools);
            }
            // Always clear the flag: a failed fetch must be retryable on the
            // next discovery round, not wedged as permanently "in flight".
            inflight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
        });
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

    /// The advertised catalog: for every allowlisted server, its discovered
    /// tool set if one has been cached, else its policy seed.
    ///
    /// Deliberately **not** intersected with the tunnels currently attached.
    /// Attachment flapping must not reach the catalog (§6.3) — a tab that is
    /// closed for a second must not make the tools vanish and reappear — so
    /// availability is reported by `call` ("browser not connected"), never by a
    /// shrinking tool list. This keeps the pre-attach discovery behaviour the
    /// static-advertise design (D4) already had.
    ///
    /// Discovery is *pull*-triggered rather than attach-triggered: a declared
    /// server with no cache entry yet gets a background `tools/list` fetch
    /// kicked off here, and its real set appears on the next discovery round.
    /// The facade re-reads the catalog on every call, so one round of staleness
    /// is the whole cost, and it avoids threading an attach hook from the
    /// gateway (which owns attach) into the root (which owns this source).
    fn tools(&self, ctx: Option<&SessionCtx>) -> Vec<Tool> {
        // Anonymous clients never reach here in practice (`requires_session`),
        // and with no channel there is nothing to discover — serve the seeds.
        let Some(ctx) = ctx else {
            return sorted(self.policy.values().flat_map(|p| p.seed.iter().cloned()));
        };

        // name -> server_id for the tunnels attached to this session. Same-name
        // duplicates cannot occur (last-attach-wins, §6.1).
        let attached: HashMap<String, String> = self
            .tunnel
            .servers(&ctx.channel_id)
            .into_iter()
            .collect();

        let mut out: Vec<Tool> = Vec::new();
        for (name, policy) in &self.policy {
            let cached = self
                .cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&(ctx.channel_id.clone(), name.clone()))
                .cloned();
            match cached {
                // `fetched ∩ allowed` — the cache narrows the catalog to what the
                // server actually publishes and can never widen it past policy.
                Some(fetched) => out.extend(
                    fetched
                        .into_iter()
                        .filter(|t| policy.allowed.contains(t.name.as_ref())),
                ),
                None => {
                    // Nothing discovered yet: advertise the seed (never empty out a
                    // seeded server) and kick off the fetch if it is attached.
                    out.extend(policy.seed.iter().cloned());
                    if let Some(server_id) = attached.get(name) {
                        self.spawn_discovery(&ctx.channel_id, name, server_id);
                    }
                }
            }
        }
        sorted(out)
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
    fn revoke(&self, token: &str) {
        // Token-specific: a late teardown must not strip the successor's live token (R1).
        self.0.revoke_token(token)
    }
}

#[cfg(test)]
mod tests {
    use super::{AcpTunnelSource, CapabilitySource, SessionCtx};
    use openab_core::mcp_proxy::AcpMcpTunnel;
    use std::collections::HashSet;
    use serde_json::{json, Map, Value};
    use std::sync::Arc;

    /// Tunnel double: reports declared servers, records forwarded `tools/call`s,
    /// and can answer or fail `tools/list`.
    struct FakeTunnel {
        servers: std::sync::Mutex<Vec<(String, String)>>,
        forwarded: std::sync::Mutex<Vec<(String, String, Value)>>,
        tools_list: std::sync::Mutex<Option<Vec<String>>>,
        tools_list_calls: std::sync::atomic::AtomicUsize,
    }

    impl FakeTunnel {
        fn with(servers: &[(&str, &str)]) -> Arc<Self> {
            Arc::new(Self {
                servers: std::sync::Mutex::new(
                    servers
                        .iter()
                        .map(|(n, i)| (n.to_string(), i.to_string()))
                        .collect(),
                ),
                forwarded: std::sync::Mutex::new(Vec::new()),
                tools_list: std::sync::Mutex::new(None),
                tools_list_calls: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        /// Make `tools/list` answer with these tool names.
        fn set_tools_list(&self, names: &[&str]) {
            *self.tools_list.lock().unwrap() =
                Some(names.iter().map(|n| n.to_string()).collect());
        }

        /// Make `tools/list` fail (no answer configured).
        fn fail_tools_list(&self) {
            *self.tools_list.lock().unwrap() = None;
        }

        fn tools_list_calls(&self) -> usize {
            self.tools_list_calls
                .load(std::sync::atomic::Ordering::SeqCst)
        }

        /// Simulate a server going away (tab closed, client disconnected).
        fn detach(&self, name: &str) {
            self.servers.lock().unwrap().retain(|(n, _)| n != name);
        }

        /// Simulate a reconnect: same declared name, freshly minted id.
        fn reattach_as(&self, name: &str, new_id: &str) {
            let mut servers = self.servers.lock().unwrap();
            servers.retain(|(n, _)| n != name);
            servers.push((name.to_string(), new_id.to_string()));
        }
    }

    #[async_trait::async_trait]
    impl AcpMcpTunnel for FakeTunnel {
        async fn call(
            &self,
            channel_id: &str,
            server_id: &str,
            method: &str,
            params: Option<Value>,
        ) -> Result<Value, String> {
            if method == "tools/list" {
                self.tools_list_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some(names) = self.tools_list.lock().unwrap().clone() else {
                    return Err("tools/list unavailable".into());
                };
                let tools: Vec<Value> = names
                    .iter()
                    .map(|n| json!({ "name": n, "inputSchema": { "type": "object" } }))
                    .collect();
                return Ok(json!({ "tools": tools }));
            }
            self.forwarded.lock().unwrap().push((
                channel_id.to_string(),
                server_id.to_string(),
                params.unwrap_or(Value::Null),
            ));
            Ok(json!({ "content": [{ "type": "text", "text": "ok" }] }))
        }

        fn servers(&self, _channel_id: &str) -> Vec<(String, String)> {
            self.servers.lock().unwrap().clone()
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
                "katashiro.click",
                "katashiro.navigate",
                "katashiro.read_dom",
                "katashiro.screenshot",
                "katashiro.type"
            ],
            "the pinned browser set is advertised regardless of attach state"
        );
    }

    #[tokio::test]
    async fn call_routes_the_name_prefix_to_the_declared_id_keeping_the_full_tool_name() {
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-abc")]);
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[]);
        let (_v, is_err) = src
            .call(Some(&ctx()), "katashiro.click", &Map::new())
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
            params["name"], "katashiro.click",
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
        // `katashiro` but publishes a tool outside its pinned five.
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-abc")]);
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[]);
        let (v, is_err) = src
            .call(Some(&ctx()), "katashiro.exec", &Map::new())
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
            .call(Some(&ctx()), "katashiro.click", &Map::new())
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
            FakeTunnel::with(&[("katashiro", "uuid-abc")]),
            &[cfg("katashiro", &["katashiro.read_dom"])],
        );
        let names: Vec<String> = src.tools(None).iter().map(|t| t.name.to_string()).collect();
        assert_eq!(
            names,
            ["katashiro.read_dom"],
            "the seed narrows to the operator's list, keeping the built-in schema"
        );
        assert!(
            src.tools(None)[0].input_schema.get("type").is_some(),
            "narrowing must not lose the built-in schema"
        );
    }

    #[tokio::test]
    async fn narrowing_the_policy_actually_refuses_the_dropped_tool() {
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-abc")]);
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[cfg("katashiro", &["katashiro.read_dom"])]);
        let (_v, is_err) = src
            .call(Some(&ctx()), "katashiro.click", &Map::new())
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
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-abc")]);
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[cfg("katashiro", &[])]);
        assert!(src.tools(None).is_empty(), "deny-all: no tools listed");
        let (_v, is_err) = src
            .call(Some(&ctx()), "katashiro.click", &Map::new())
            .await
            .unwrap();
        assert!(is_err);
        assert!(tunnel.forwarded.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn configuring_other_servers_drops_the_browser_default() {
        // Writing any entry takes over the allowlist wholesale — browser is not
        // silently retained alongside the operator's list.
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-abc")]);
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[cfg("notes", &["notes.list"])]);
        let (_v, is_err) = src
            .call(Some(&ctx()), "katashiro.click", &Map::new())
            .await
            .unwrap();
        assert!(is_err, "browser is no longer allowlisted once config is written");
        assert!(tunnel.forwarded.lock().unwrap().is_empty());
    }

    /// Let any spawned discovery task run to completion.
    async fn settle() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn discovery_fills_the_catalog_for_a_name_only_server() {
        // `notes` was admitted by name alone, so it starts with no seed. After a
        // discovery round its real tools/list is cached and appears.
        let tunnel = FakeTunnel::with(&[("notes", "uuid-n")]);
        tunnel.set_tools_list(&["notes.list", "notes.get"]);
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[cfg("notes", &["notes.list"])]);

        assert!(src.tools(Some(&ctx())).is_empty(), "nothing discovered yet");
        settle().await;

        let names: Vec<String> = src
            .tools(Some(&ctx()))
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        assert_eq!(
            names,
            ["notes.list"],
            "fetched ∩ allowed — notes.get was published but never permitted"
        );
    }

    #[tokio::test]
    async fn caching_is_never_itself_a_grant() {
        // The server publishes a tool the operator did not permit. Caching it
        // must not make it visible OR callable.
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-abc")]);
        tunnel.set_tools_list(&["katashiro.read_dom", "katashiro.exec"]);
        let src =
            AcpTunnelSource::with_config(tunnel.clone(), &[cfg("katashiro", &["katashiro.read_dom"])]);
        let _ = src.tools(Some(&ctx()));
        settle().await;

        let names: Vec<String> = src
            .tools(Some(&ctx()))
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        assert_eq!(names, ["katashiro.read_dom"], "the unpermitted tool stays out");

        let (_v, is_err) = src
            .call(Some(&ctx()), "katashiro.exec", &Map::new())
            .await
            .unwrap();
        assert!(is_err, "and it stays uncallable even though it was cached");
    }

    #[tokio::test]
    async fn a_seeded_server_keeps_its_seed_until_discovery_succeeds() {
        // Fetch fails: the catalog must fall back to the seed, never to empty.
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-abc")]);
        tunnel.fail_tools_list();
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[]);
        assert_eq!(src.tools(Some(&ctx())).len(), 5);
        settle().await;
        assert_eq!(
            src.tools(Some(&ctx())).len(),
            5,
            "a failed discovery must not empty out a seeded server"
        );
    }

    #[tokio::test]
    async fn discovery_is_not_repeated_while_one_is_in_flight() {
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-abc")]);
        tunnel.set_tools_list(&["katashiro.read_dom"]);
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[]);
        for _ in 0..5 {
            let _ = src.tools(Some(&ctx()));
        }
        settle().await;
        assert_eq!(
            tunnel.tools_list_calls(),
            1,
            "repeated discovery rounds must not pile up tools/list requests"
        );
    }

    #[tokio::test]
    async fn cached_tools_survive_a_reconnect_that_changes_the_server_id() {
        // The whole point of keying the cache by NAME: the client mints a new id
        // on reconnect, and an id-keyed entry would be orphaned by it.
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-old")]);
        tunnel.set_tools_list(&["katashiro.read_dom"]);
        let src = AcpTunnelSource::with_config(tunnel.clone(), &[]);
        let _ = src.tools(Some(&ctx()));
        settle().await;
        assert_eq!(src.tools(Some(&ctx())).len(), 1);

        tunnel.reattach_as("katashiro", "uuid-new");
        assert_eq!(
            src.tools(Some(&ctx())).len(),
            1,
            "the discovered set must survive the id change"
        );
    }

    // --- F6: two client-declared servers in one session ---
    //
    // The multi-server claim §6.2 makes, exercised through the real source: one
    // session declares `browser` and a second, non-browser server; both are
    // discovered and callable, tool names do not collide, and each server's
    // policy is enforced independently. The agent-side leg (facade meta-tools →
    // this source) is not covered here — facade mode is not live anywhere yet,
    // which is the same precondition F5 is blocked on.

    fn two_server_src(tunnel: Arc<FakeTunnel>) -> AcpTunnelSource {
        AcpTunnelSource::with_config(
            tunnel,
            &[
                cfg("katashiro", &["katashiro.click", "katashiro.read_dom"]),
                cfg("notes", &["notes.list"]),
            ],
        )
    }

    #[tokio::test]
    async fn two_declared_servers_are_both_discovered_without_collision() {
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-b"), ("notes", "uuid-n")]);
        // Both servers publish a tool literally called `list` under their own
        // prefix — the case a naive un-prefixed catalog would collapse.
        tunnel.set_tools_list(&["katashiro.click", "katashiro.read_dom", "notes.list"]);
        let src = two_server_src(tunnel.clone());

        let _ = src.tools(Some(&ctx()));
        settle().await;

        let names: Vec<String> = src
            .tools(Some(&ctx()))
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        assert_eq!(
            names,
            ["katashiro.click", "katashiro.read_dom", "notes.list"],
            "both servers contribute, each under its own prefix"
        );
        assert_eq!(
            names.len(),
            names.iter().collect::<HashSet<_>>().len(),
            "no duplicate tool names across servers"
        );
    }

    #[tokio::test]
    async fn each_server_receives_only_its_own_calls() {
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-b"), ("notes", "uuid-n")]);
        let src = two_server_src(tunnel.clone());

        for tool in ["katashiro.click", "notes.list"] {
            let (_v, is_err) = src.call(Some(&ctx()), tool, &Map::new()).await.unwrap();
            assert!(!is_err, "{tool} should dispatch");
        }

        let fwd = tunnel.forwarded.lock().unwrap();
        let routed: Vec<(String, String)> = fwd
            .iter()
            .map(|(_c, id, params)| (id.clone(), params["name"].as_str().unwrap().to_string()))
            .collect();
        assert_eq!(
            routed,
            [
                ("uuid-b".to_string(), "katashiro.click".to_string()),
                ("uuid-n".to_string(), "notes.list".to_string())
            ],
            "each tool reaches the tunnel of the server that declared it"
        );
    }

    #[tokio::test]
    async fn one_servers_policy_does_not_leak_to_another() {
        // `katashiro.click` is permitted, `notes.click` is not — a per-server gate,
        // not a global tool-name gate.
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-b"), ("notes", "uuid-n")]);
        let src = two_server_src(tunnel.clone());

        let (_v, ok_err) = src
            .call(Some(&ctx()), "katashiro.click", &Map::new())
            .await
            .unwrap();
        assert!(!ok_err);

        let (v, denied) = src
            .call(Some(&ctx()), "notes.click", &Map::new())
            .await
            .unwrap();
        assert!(denied, "notes may not borrow browser's permissions");
        assert!(v["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("is not permitted"));
        assert_eq!(
            tunnel.forwarded.lock().unwrap().len(),
            1,
            "only the permitted call reached a tunnel"
        );
    }

    #[tokio::test]
    async fn one_server_detaching_leaves_the_other_callable() {
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-b"), ("notes", "uuid-n")]);
        let src = two_server_src(tunnel.clone());
        tunnel.detach("katashiro");

        let (_v, browser_err) = src
            .call(Some(&ctx()), "katashiro.click", &Map::new())
            .await
            .unwrap();
        assert!(browser_err, "the detached server reports not connected");

        let (_v, notes_err) = src
            .call(Some(&ctx()), "notes.list", &Map::new())
            .await
            .unwrap();
        assert!(!notes_err, "its neighbour is unaffected");
    }

    #[tokio::test]
    async fn malformed_tool_name_without_a_prefix_is_rejected() {
        let src = AcpTunnelSource::with_config(FakeTunnel::with(&[("katashiro", "uuid-abc")]), &[]);
        let (v, is_err) = src.call(Some(&ctx()), "click", &Map::new()).await.unwrap();
        assert!(is_err);
        assert!(v["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("expected <server>.<tool>"));
    }
}

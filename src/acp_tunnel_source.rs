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
//! **Routing is by what the server published, not by the tool name's shape**
//! (F5). A tool is routed to the server whose discovered `tools/list` contained
//! it — a generic server may publish a bare `build`, or a name whose first
//! segment is not its own, and both stay callable. The declared server `name`
//! (from `{type:"acp", id, name}`; the client mints a fresh `id` per connection
//! while `name` is stable) then resolves to `(channel_id, id)` through the
//! registry's `resolve_by_name`, and the **full** published tool name is
//! forwarded unchanged because the server's own `tools/call` expects it. The
//! `<server>.` prefix is only a pre-discovery fallback for routing.
//!
//! **Admission is the transport, not an allowlist** (D-29, reversing D-20).
//! There is no operator `[[mcp.acp_servers]]` gate: any server that authenticates
//! to `/acp` and declares itself may publish every tool it lists, because the
//! extension already authenticates to reach the tunnel and a second allowlist
//! duplicated that intent. Discovery is therefore driven by what is *attached*,
//! not by a configured list — `tools()` enumerates `attached_server_names` and
//! resolves each to an id through `resolve_by_name`. Names only, one collapse
//! rule: the enumerating `tunnel.servers(channel_id)` deleted in `74315a60`
//! (which collapsed same-name entries in registry order rather than by
//! generation, resolving to a stale tunnel) is *not* revived — the new
//! enumerator returns names, and the single `resolve_by_name` still does every
//! name → id resolution beside the eviction that makes it unique.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use openab_core::acp_mcp::AcpMcpTunnel;
use openab_mcp::mcp::sources::{CapabilitySource, SessionCtx};
use openab_mcp::rmcp::model::Tool;
use serde_json::{json, Map, Value};

/// Facade capability source backed by MCP-over-ACP tunnels to client-declared
/// MCP servers.
pub struct AcpTunnelSource {
    tunnel: Arc<dyn AcpMcpTunnel>,
    /// Discovered tool sets, keyed `(channel_id, declared_name)` (§6.3).
    ///
    /// Keyed by **name**, not by `server_id` as an earlier draft of §6.3 said:
    /// the client mints a fresh id per connection, so an id-keyed entry would be
    /// orphaned by the very reconnect the cache exists to paper over. Keying by
    /// name is what lets a discovered set survive a reconnect, which is the
    /// cache's entire purpose.
    ///
    /// Holds what the server published, and that is what is advertised — with the
    /// allowlist gone (D-29) there is no read-time filter to narrow it.
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
    /// Source over the MCP-over-ACP tunnel. No operator allowlist (D-29): every
    /// server that authenticates to `/acp` and declares itself may publish the
    /// tools it lists, so what is admitted is decided by the transport, not by
    /// config. The catalog is driven by what is actually attached (see
    /// [`AcpTunnelSource::tools`]).
    pub fn new(tunnel: Arc<dyn AcpMcpTunnel>) -> Self {
        Self {
            tunnel,
            cache: Arc::new(std::sync::Mutex::new(HashMap::new())),
            inflight: Arc::new(std::sync::Mutex::new(HashSet::new())),
        }
    }

    /// Fetch one server's real `tools/list` in the background and cache it
    /// (§6.3). Cheap to call on every discovery round: it is a no-op while a
    /// fetch for the same `(channel, name)` is already in flight.
    ///
    /// What lands in the cache is what the server published, and since D-29 that
    /// is also what is advertised — there is no allowlist to intersect against on
    /// read. A tool appears in the catalog once discovery has fetched it and the
    /// server is (or was) attached; see [`AcpTunnelSource::tools`].
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

    /// The declared name of the connected server that published `tool` on `channel_id`, from the
    /// discovery cache.
    ///
    /// This is how a tool is routed since F5 dropped the `<server>.<tool>` name-prefix assumption: a
    /// generic server may publish a bare `build`, or a name whose first segment is not its own, so
    /// the publisher is looked up by what was actually discovered rather than parsed out of the tool
    /// string. Deterministic on the (already-ambiguous) case of two servers publishing the same bare
    /// name — the lexicographically-first server name wins — so routing never depends on `HashMap`
    /// iteration order.
    fn server_publishing(&self, channel_id: &str, tool: &str) -> Option<String> {
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let publishers: Vec<String> = cache
            .iter()
            .filter(|((c, _), tools)| c == channel_id && tools.iter().any(|t| t.name.as_ref() == tool))
            .map(|((_, name), _)| name.clone())
            .collect();
        // Shadowing tiebreak (D-34): if the tool is `<prefix>.<...>` and one publisher IS `<prefix>`,
        // prefer it — so a tool published under its namesake server's prefix routes to that server,
        // not to a same-name impostor that merely sorts earlier. Content routing + no allowlist
        // (D-29) + keyless-loopback (D-30) let a second local server attach and publish the same
        // literal name; this restores the prefix's authority as a TIEBREAK without reintroducing the
        // old hard `<server>.<tool>` requirement (a bare/unnamespaced name still routes by publisher).
        // Key-gated: moot once `OPENAB_ACP_AUTH_KEY` is set. A truly bare name has no prefix to appeal
        // to, so it still falls to the deterministic lexicographic-min — inherent to unnamespaced names.
        if let Some((prefix, _)) = tool.split_once('.') {
            if publishers.iter().any(|n| n == prefix) {
                return Some(prefix.to_string());
            }
        }
        publishers.into_iter().min()
    }

    /// The `<server>` segment of a `<server>.<tool>` name, as a **pre-discovery fallback** for
    /// routing. Once discovery has run, [`server_publishing`] is authoritative and this is not
    /// consulted; before it, a prefixed name still yields a candidate server so a cold call reports
    /// "not connected" (and the common browser case works) rather than "not available". A bare or
    /// mismatched-prefix name has no candidate here and waits for discovery.
    fn split_prefix(tool: &str) -> Option<&str> {
        let (server, _rest) = tool.split_once('.')?;
        if server.is_empty() {
            return None;
        }
        Some(server)
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

    /// The advertised catalog: every tool discovered from the servers connected
    /// to this session, unfiltered (D-29 removed the allowlist — a connected
    /// server publishes every tool it declares).
    ///
    /// Which servers to advertise is driven by what is **attached**, since there
    /// is no configured list any more: the set of names is
    /// `attached_server_names` UNION the names already discovered for this
    /// channel. The union is what preserves §6.3 — attachment flapping must not
    /// reach the catalog, so a tab closed for a second stays in the catalog via
    /// its cache entry even while it is momentarily not in `attached_server_names`.
    /// A newly attached server, conversely, is what introduces a new name.
    ///
    /// COLD START (§6.3, unchanged by D-29): with no seed, an attached server
    /// publishes nothing until its first `tools/list` returns. Discovery is
    /// *pull*-triggered — an attached server with no cache entry gets a
    /// background `tools/list` fetch kicked off here, and its real set appears on
    /// the next discovery round. The facade re-reads the catalog on every call,
    /// so one round of staleness is the whole cost, and it avoids threading an
    /// attach hook from the gateway (which owns attach) into the root.
    fn tools(&self, ctx: Option<&SessionCtx>) -> Vec<Tool> {
        // Anonymous clients never reach here in practice (`requires_session`), and
        // with no channel there is nothing attached and nothing to discover.
        let Some(ctx) = ctx else {
            return Vec::new();
        };

        // Snapshot this channel's discovered catalog once, rather than re-locking per name.
        let cached: HashMap<String, Vec<Tool>> = {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            cache
                .iter()
                .filter(|((c, _), _)| *c == ctx.channel_id)
                .map(|((_, name), tools)| (name.clone(), tools.clone()))
                .collect()
        };

        // Names to advertise: attached now, UNION already-discovered (§6.3 no-shrink).
        let mut names: HashSet<String> =
            self.tunnel.attached_server_names(&ctx.channel_id).into_iter().collect();
        names.extend(cached.keys().cloned());

        let mut out: Vec<Tool> = Vec::new();
        for name in &names {
            match cached.get(name) {
                // Unfiltered: with the allowlist gone, every tool the server published is admitted.
                Some(fetched) => out.extend(fetched.iter().cloned()),
                None => {
                    // Attached but not discovered yet — the cold-start window. Resolve through the
                    // same route calls use (one resolution rule, §6.1); a name that appears only
                    // because it is still cached but detached resolves to None here and simply is
                    // not re-fetched, while its cached tools above keep it in the catalog.
                    if let Some(server_id) = self.tunnel.resolve_by_name(&ctx.channel_id, name) {
                        self.spawn_discovery(&ctx.channel_id, name, &server_id);
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

        // Route by what was DISCOVERED, not by parsing the tool name (F5). A generic server may
        // publish a bare `build` or a name whose first segment is not the server's own; both are
        // callable because the publisher is looked up from the cache. The `<server>.` prefix is only
        // a pre-discovery fallback (see `split_prefix`). No allowlist gate (D-29): the only refusal
        // left is not-connected, a liveness answer.
        let Some(server_name) = self
            .server_publishing(&ctx.channel_id, tool)
            .or_else(|| Self::split_prefix(tool).map(str::to_string))
        else {
            return Ok(Self::error_result(format!(
                "tool {tool:?} is not available: no connected server has published it, and it \
                 carries no <server>.<tool> prefix to route by before discovery"
            )));
        };

        // Resolve the declared name to the tunnel's registry key (§6.1). Delegated rather than
        // done here: enumerating and taking the first name match is only correct while same-name
        // entries cannot coexist, and that uniqueness is maintained in the gateway, out of this
        // file's sight. A "take the first" caller does not fail when it breaks — it silently routes
        // to an arbitrary tunnel. The resolution now lives beside the eviction that guarantees it.
        let Some(server_id) = self.tunnel.resolve_by_name(&ctx.channel_id, &server_name) else {
            return Ok(Self::error_result(format!(
                "{server_name} not connected: open the OpenAB side panel in your browser"
            )));
        };

        let params = json!({ "name": tool, "arguments": args });
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

impl openab_core::acp_mcp::SessionTokenRegistrar for FacadeRegistrar {
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
    use openab_core::acp_mcp::AcpMcpTunnel;
    use std::collections::HashSet;
    use serde_json::{json, Map, Value};
    use std::sync::Arc;

    /// Tunnel double: reports declared servers, records forwarded `tools/call`s,
    /// and can answer or fail `tools/list`.
    struct FakeTunnel {
        servers: std::sync::Mutex<Vec<(String, String)>>,
        forwarded: std::sync::Mutex<Vec<(String, String, Value)>>,
        tools_list: std::sync::Mutex<Option<Vec<String>>>,
        /// Per-declared-name tool lists returned verbatim (no prefix partition), so a test can give
        /// a server an unprefixed tool (`build`) or a name whose first segment is not the server's.
        server_tools: std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>,
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
                server_tools: std::sync::Mutex::new(std::collections::HashMap::new()),
                tools_list_calls: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        /// Make `tools/list` answer with these tool names.
        fn set_tools_list(&self, names: &[&str]) {
            *self.tools_list.lock().unwrap() =
                Some(names.iter().map(|n| n.to_string()).collect());
        }

        /// Make a specific server's `tools/list` return exactly `names`, verbatim — the tool names
        /// need not be prefixed with the server's name.
        fn set_server_tools(&self, server_name: &str, names: &[&str]) {
            self.server_tools
                .lock()
                .unwrap()
                .insert(server_name.to_string(), names.iter().map(|n| n.to_string()).collect());
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
        /// The double holds a controlled list, so matching it directly is honest here — the
        /// reason the real implementation delegates to the gateway is that IT cannot rank two
        /// same-name tunnels, not that matching is wrong in principle.
        fn resolve_by_name(&self, _channel_id: &str, server_name: &str) -> Option<String> {
            self.servers
                .lock()
                .unwrap()
                .iter()
                .find(|(name, _)| name == server_name)
                .map(|(_, id)| id.clone())
        }

        fn attached_server_names(&self, _channel_id: &str) -> Vec<String> {
            let mut names: Vec<String> = self
                .servers
                .lock()
                .unwrap()
                .iter()
                .map(|(name, _)| name.clone())
                .collect();
            names.sort();
            names.dedup();
            names
        }

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
                // The name for this `server_id` comes from the registry.
                let server_name = self
                    .servers
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(_, id)| id == server_id)
                    .map(|(n, _)| n.clone());
                // Prefer an explicit per-server list (returned verbatim, so it may be unprefixed);
                // otherwise fall back to the shared list partitioned by the server's declared name.
                // A real server returns only ITS OWN tools; the partition keeps one server's tunnel
                // from appearing to publish another's now that the source applies no allowlist.
                let explicit = server_name
                    .as_ref()
                    .and_then(|n| self.server_tools.lock().unwrap().get(n).cloned());
                let names = match explicit {
                    Some(list) => list,
                    None => {
                        let Some(shared) = self.tools_list.lock().unwrap().clone() else {
                            return Err("tools/list unavailable".into());
                        };
                        shared
                            .into_iter()
                            .filter(|n| match &server_name {
                                Some(name) => n
                                    .split_once('.')
                                    .is_some_and(|(prefix, _)| prefix == name),
                                None => true,
                            })
                            .collect()
                    }
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
    }

    fn ctx() -> SessionCtx {
        SessionCtx {
            channel_id: "acp_x".into(),
        }
    }

    /// Let any spawned discovery task run to completion.
    async fn settle() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    // --- Routing (unchanged by D-29): the prefix selects the tunnel by NAME ---

    #[tokio::test]
    async fn call_routes_the_name_prefix_to_the_declared_id_keeping_the_full_tool_name() {
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-abc")]);
        let src = AcpTunnelSource::new(tunnel.clone());
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

    // Inverted by F5: a bare name is no longer "malformed" — bare tools are legitimate and routed by
    // discovery. What a bare name that NO connected server published earns is "not available", not a
    // format complaint. (The tool here is never discovered — katashiro's tools/list is not fetched —
    // and "click" has no prefix to fall back on, so both routing paths miss.)
    #[tokio::test]
    async fn an_undiscovered_bare_tool_is_reported_unavailable() {
        let src = AcpTunnelSource::new(FakeTunnel::with(&[("katashiro", "uuid-abc")]));
        let (v, is_err) = src.call(Some(&ctx()), "click", &Map::new()).await.unwrap();
        assert!(is_err);
        assert!(v["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("is not available"));
    }

    // --- Admission is the transport, not an allowlist (D-29, reversing D-20) ---

    #[tokio::test]
    async fn any_attached_server_is_callable_without_an_allowlist() {
        // Inverted from `unlisted_server_name_is_refused_even_when_a_tunnel_is_attached`. With the
        // operator allowlist gone (D-29), a server that authenticated to `/acp` and attached is
        // admitted by the transport: its declared tool routes to its tunnel, not refused as
        // "not in the allowlist".
        let tunnel = FakeTunnel::with(&[("notes", "uuid-n")]);
        let src = AcpTunnelSource::new(tunnel.clone());
        let (_v, is_err) = src
            .call(Some(&ctx()), "notes.anything", &Map::new())
            .await
            .unwrap();
        assert!(!is_err, "no allowlist gate: a connected server's declared tool dispatches");
        let fwd = tunnel.forwarded.lock().unwrap();
        assert_eq!(fwd[0].1, "uuid-n");
        assert_eq!(fwd[0].2["name"], "notes.anything");
    }

    #[tokio::test]
    async fn every_tool_a_server_publishes_is_advertised_unfiltered() {
        // Inverted from `caching_is_never_itself_a_grant`, which asserted that a published-but-
        // unpermitted tool stayed OUT of the catalog and uncallable. There is no permit step any
        // more: whatever a connected server publishes is advertised and callable. (Also supersedes
        // `unpinned_tool_on_an_allowlisted_server_is_refused` — the "pinning" it guarded is gone.)
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-abc")]);
        tunnel.set_tools_list(&["katashiro.read_dom", "katashiro.exec"]);
        let src = AcpTunnelSource::new(tunnel.clone());
        let _ = src.tools(Some(&ctx()));
        settle().await;

        let names: Vec<String> = src
            .tools(Some(&ctx()))
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        assert_eq!(
            names,
            ["katashiro.exec", "katashiro.read_dom"],
            "both published tools appear — nothing is filtered out"
        );

        let (_v, is_err) = src
            .call(Some(&ctx()), "katashiro.exec", &Map::new())
            .await
            .unwrap();
        assert!(!is_err, "and the once-unpermitted tool is now callable");
    }

    // --- Liveness: not-connected is the only refusal left, and it is a liveness answer ---

    #[tokio::test]
    async fn an_unattached_server_reports_not_connected() {
        // Renamed from `allowlisted_but_unattached_server_reports_not_connected`. The refusal that
        // survives D-29 is liveness, not permission: a name with no attached tunnel resolves to
        // nothing.
        let tunnel = FakeTunnel::with(&[]);
        let src = AcpTunnelSource::new(tunnel.clone());
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

    // --- Discovery drives the catalog from what is ATTACHED (§6.3, cold start) ---

    #[tokio::test]
    async fn an_attached_server_has_no_schema_until_discovery_supplies_one() {
        // Renamed from `an_allowlisted_tool_has_no_schema_until_discovery_supplies_one`. The
        // cold-start window survives D-29; what admits the server is now attachment, not config, so
        // the catalog is empty until the first `tools/list` returns over the tunnel.
        let tunnel = FakeTunnel::with(&[("notes", "uuid-n")]);
        let src = AcpTunnelSource::new(tunnel.clone());
        assert!(
            src.tools(Some(&ctx())).is_empty(),
            "attached but not yet discovered: nothing to advertise until tools/list is fetched"
        );
    }

    #[tokio::test]
    async fn discovery_fills_the_catalog_for_an_attached_server() {
        // Renamed from `discovery_fills_the_catalog_for_a_name_only_server`, and the assertion
        // inverted: BOTH published tools now appear. The old test expected `["notes.list"]` because
        // `notes.get` was "published but never permitted"; with no allowlist there is nothing to
        // intersect against.
        let tunnel = FakeTunnel::with(&[("notes", "uuid-n")]);
        tunnel.set_tools_list(&["notes.list", "notes.get"]);
        let src = AcpTunnelSource::new(tunnel.clone());

        assert!(src.tools(Some(&ctx())).is_empty(), "nothing discovered yet");
        settle().await;

        let names: Vec<String> = src
            .tools(Some(&ctx()))
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        assert_eq!(
            names,
            ["notes.get", "notes.list"],
            "every published tool appears — no allowlist to intersect against"
        );
    }

    #[tokio::test]
    async fn a_discovered_catalog_survives_detachment() {
        // §6.3: attachment flapping stays out of the catalog. Discovery is driven by
        // `attached_server_names`, but a name already in the cache keeps its tools even while it is
        // momentarily not attached — the UNION in `tools()` is what preserves this.
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-abc")]);
        tunnel.set_tools_list(&["katashiro.click", "katashiro.navigate", "katashiro.read_dom", "katashiro.screenshot", "katashiro.type"]);
        let src = AcpTunnelSource::new(tunnel.clone());
        let _ = src.tools(Some(&ctx()));
        settle().await;
        let before: Vec<String> =
            src.tools(Some(&ctx())).iter().map(|t| t.name.to_string()).collect();
        assert!(!before.is_empty(), "precondition: discovery must have populated the catalog");

        tunnel.detach("katashiro");
        let after: Vec<String> =
            src.tools(Some(&ctx())).iter().map(|t| t.name.to_string()).collect();
        assert_eq!(after, before, "a detached tunnel must not shrink the catalog");
    }

    #[tokio::test]
    async fn a_failed_discovery_does_not_empty_an_already_populated_catalog() {
        // Once a catalog has been discovered, a LATER failed fetch must not empty it — the
        // invariant that protects a running deployment through a flap.
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-abc")]);
        tunnel.set_tools_list(&["katashiro.click", "katashiro.navigate", "katashiro.read_dom", "katashiro.screenshot", "katashiro.type"]);
        let src = AcpTunnelSource::new(tunnel.clone());

        let _ = src.tools(Some(&ctx()));
        settle().await;
        let discovered = src.tools(Some(&ctx())).len();
        assert!(discovered > 0, "precondition: discovery must have populated the catalog");

        tunnel.fail_tools_list();
        let _ = src.tools(Some(&ctx()));
        settle().await;
        assert_eq!(
            src.tools(Some(&ctx())).len(),
            discovered,
            "a failed discovery must not empty a catalog that was already populated"
        );
    }

    #[tokio::test]
    async fn discovery_is_not_repeated_while_one_is_in_flight() {
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-abc")]);
        tunnel.set_tools_list(&["katashiro.read_dom"]);
        let src = AcpTunnelSource::new(tunnel.clone());
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
        // The whole point of keying the cache by NAME: the client mints a new id on reconnect, and
        // an id-keyed entry would be orphaned by it.
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-old")]);
        tunnel.set_tools_list(&["katashiro.read_dom"]);
        let src = AcpTunnelSource::new(tunnel.clone());
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
    // The multi-server claim §6.2 makes, exercised through the real source: one session declares
    // `katashiro` and a second, non-browser server; both are discovered and callable, tool names do
    // not collide, and each tool routes to the tunnel of the server that declared it. The agent-side
    // leg (facade meta-tools → this source) is not covered here — facade mode is not live anywhere
    // yet.

    fn two_server_src(tunnel: Arc<FakeTunnel>) -> AcpTunnelSource {
        AcpTunnelSource::new(tunnel)
    }

    #[tokio::test]
    async fn two_declared_servers_are_both_discovered_without_collision() {
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-b"), ("notes", "uuid-n")]);
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
    async fn a_tool_routes_by_its_prefix_to_the_named_server() {
        // Inverted from `one_servers_policy_does_not_leak_to_another`. There is no per-server policy
        // gate to leak any more; what remains, and is worth pinning, is that the PREFIX selects the
        // tunnel: `notes.click` reaches `notes`'s tunnel (whether `notes` implements `click` is the
        // server's concern, not openab's), never `katashiro`'s.
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-b"), ("notes", "uuid-n")]);
        let src = two_server_src(tunnel.clone());

        let (_v, err) = src
            .call(Some(&ctx()), "notes.click", &Map::new())
            .await
            .unwrap();
        assert!(!err, "prefix selects the tunnel; the server decides what it implements");
        let fwd = tunnel.forwarded.lock().unwrap();
        assert_eq!(fwd.len(), 1);
        assert_eq!(fwd[0].1, "uuid-n", "routed to notes' tunnel, not katashiro's");
        assert_eq!(fwd[0].2["name"], "notes.click");
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

    // --- F5: routing by what was discovered, not by the tool name's shape ---

    #[tokio::test]
    async fn an_unprefixed_tool_from_a_generic_server_is_callable() {
        // A generic server "project-tools" publishing a bare "build" (no <server>. prefix) is both
        // discoverable and callable. The old `split_prefix` rejected it as malformed; discovery
        // routing looks the publisher up from the cache.
        let tunnel = FakeTunnel::with(&[("project-tools", "uuid-p")]);
        tunnel.set_server_tools("project-tools", &["build"]);
        let src = AcpTunnelSource::new(tunnel.clone());

        let _ = src.tools(Some(&ctx()));
        settle().await;
        let names: Vec<String> =
            src.tools(Some(&ctx())).iter().map(|t| t.name.to_string()).collect();
        assert_eq!(names, ["build"], "the bare tool is advertised");

        let (_v, is_err) = src.call(Some(&ctx()), "build", &Map::new()).await.unwrap();
        assert!(!is_err, "a bare tool must dispatch, not be rejected as malformed");
        let fwd = tunnel.forwarded.lock().unwrap();
        assert_eq!(fwd[0].1, "uuid-p", "routed to the publishing server's tunnel");
        assert_eq!(fwd[0].2["name"], "build", "the original tool name is forwarded unchanged");
    }

    #[tokio::test]
    async fn a_prefixed_tool_routes_to_its_namesake_not_a_same_name_impostor() {
        // F5 shadowing mitigation (D-34): two servers publish the EXACT tool `katashiro.click` — the
        // real one named `katashiro` and an impostor `aaa` that sorts earlier for min(). The tool's
        // prefix (`katashiro`) matches the real server's name, so routing prefers it over the
        // lexicographically-earlier impostor. Key-gated: moot once OPENAB_ACP_AUTH_KEY is set.
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-k"), ("aaa", "uuid-a")]);
        tunnel.set_server_tools("katashiro", &["katashiro.click"]);
        tunnel.set_server_tools("aaa", &["katashiro.click"]);
        let src = AcpTunnelSource::new(tunnel.clone());

        let _ = src.tools(Some(&ctx()));
        settle().await;

        let (_v, is_err) = src.call(Some(&ctx()), "katashiro.click", &Map::new()).await.unwrap();
        assert!(!is_err);
        let fwd = tunnel.forwarded.lock().unwrap();
        assert_eq!(
            fwd[0].1, "uuid-k",
            "the prefix's namesake wins the tiebreak; the earlier-sorting impostor 'aaa' must not shadow it"
        );
    }

    #[tokio::test]
    async fn a_tool_whose_prefix_differs_from_its_server_name_is_callable() {
        // Server "katashiro" publishing "browser.click": the first segment ("browser") is not the
        // server's name. `split_prefix` would route to a phantom "browser" server; discovery routing
        // sends it to katashiro, its actual publisher.
        let tunnel = FakeTunnel::with(&[("katashiro", "uuid-k")]);
        tunnel.set_server_tools("katashiro", &["browser.click"]);
        let src = AcpTunnelSource::new(tunnel.clone());

        let _ = src.tools(Some(&ctx()));
        settle().await;

        let (_v, is_err) = src.call(Some(&ctx()), "browser.click", &Map::new()).await.unwrap();
        assert!(!is_err, "a mismatched-prefix tool must route to its publisher, not a phantom server");
        let fwd = tunnel.forwarded.lock().unwrap();
        assert_eq!(fwd[0].1, "uuid-k", "routed to katashiro, not the 'browser' prefix");
        assert_eq!(fwd[0].2["name"], "browser.click", "the original tool name is forwarded unchanged");
    }
}

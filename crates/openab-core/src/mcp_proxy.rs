//! Agent-facing wiring for MCP-over-ACP browser control (feature `acp-mcp`).
//!
//! The module name is historical. It no longer hosts a proxy: the per-session loopback MCP
//! server, its bearer and its per-session config rewrite were removed along with the stdio
//! bridge, leaving the OAB MCP Facade as the only way an agent reaches browser tools.
//!
//! What remains is the seam between core and the colocated agent CLI:
//!
//! - [`browser_tools`] — the static tool set (D4 static-advertise), now consumed by the facade's
//!   capability source rather than served here. It is advertised whether or not an extension is
//!   attached; a call while disconnected reports "browser not connected" instead of the tools
//!   silently disappearing.
//! - [`AcpMcpTunnel`] — the trait core calls to reach a session's tunnel, implemented in the root.
//! - [`write_facade_mcp_config`] — writes the one static facade entry into each colocated CLI's
//!   config, and retires the bridge entry it replaces.
//! - [`report_browser_control`] — startup report of whether browser control is on, plus the
//!   migration notice for the removed `OPENAB_BROWSER_MODE`.

use rmcp::model::{object, Tool};
use serde_json::{json, Value};

/// Core-side interface to the browser MCP-over-ACP tunnel (D6-a'). Implemented by the ROOT
/// (which bridges to the gateway's per-connection tunnel registry) and consumed by the MCP
/// proxy here. Keeping the trait in core with the impl in root preserves the core/gateway
/// sibling independence, matching the existing `ChatAdapter` pattern.
#[async_trait::async_trait]
pub trait AcpMcpTunnel: Send + Sync {
    /// Forward an inner MCP request (e.g. `tools/call`) to the client MCP server identified by
    /// `(channel_id, server_id)` and return the inner MCP result payload. Err if no matching
    /// tunnel is currently attached to that session.
    ///
    /// `server_id` selects among multiple `type:acp` servers on one session (compound-key
    /// registry, P1). During the single-browser transition an empty `server_id` is a sentinel
    /// meaning "the sole tunnel on this channel" — the proxy/bridge callers don't yet know the
    /// client-declared id at spawn time (real per-server routing lands in P2).
    async fn call(
        &self,
        channel_id: &str,
        server_id: &str,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, String>;

    /// The `type:acp` servers currently registered for `channel_id`, as `(declared_name,
    /// server_id)` pairs.
    ///
    /// Both halves are needed and they are *not* interchangeable (ADR §6.1): the registry is keyed
    /// by the client-minted `server_id`, which the reference client mints as a fresh UUID **per
    /// connection**, while a tool name carries the stable declared **name** (`katashiro.click`) and
    /// the §6.4 trust gate is keyed by that name too. Enumerating both is what lets a capability
    /// source resolve a tool prefix back to a tunnel; matching a prefix against the registry key
    /// alone can never work.
    ///
    /// Sync because implementations just read an in-memory registry. The default is empty, so
    /// implementations that track no declarations (test doubles, single-target bridges) simply
    /// advertise nothing.
    fn servers(&self, _channel_id: &str) -> Vec<(String, String)> {
        Vec::new()
    }
}

/// The fixed set of browser tools OpenAB advertises over MCP (D4 static-advertise). DOM-
/// semantic actions the extension executes in the user's active tab; model-agnostic.
pub fn browser_tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "katashiro.click",
            "Click the element matching a CSS selector in the active browser tab.",
            object(json!({
                "type": "object",
                "properties": { "selector": { "type": "string", "description": "CSS selector" } },
                "required": ["selector"]
            })),
        ),
        Tool::new(
            "katashiro.read_dom",
            "Read a snapshot of the active tab's DOM (optionally scoped to a selector).",
            object(json!({
                "type": "object",
                "properties": { "selector": { "type": "string", "description": "optional CSS selector to scope the snapshot" } }
            })),
        ),
        Tool::new(
            "katashiro.navigate",
            "Navigate the active browser tab to a URL.",
            object(json!({
                "type": "object",
                "properties": { "url": { "type": "string", "description": "absolute URL" } },
                "required": ["url"]
            })),
        ),
        Tool::new(
            "katashiro.type",
            "Type text into the element matching a CSS selector in the active tab.",
            object(json!({
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "CSS selector" },
                    "text": { "type": "string", "description": "text to type" }
                },
                "required": ["selector", "text"]
            })),
        ),
        Tool::new(
            "katashiro.screenshot",
            "Capture a screenshot of the active browser tab.",
            object(json!({ "type": "object", "properties": {} })),
        ),
    ]
}


/// Write `bytes` to `path`, then tighten it to owner-only (0600). The file holds a live bearer
/// token for the loopback MCP server, so it must not be group/world readable.
async fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    tokio::fs::write(path, bytes).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    }
    Ok(())
}

/// True when an `openab-browser` entry is one we can **prove** we wrote, and so may be removed.
///
/// Only the removed bridge entry qualifies. It was byte-identical every session
/// (`{"command":"openab","args":["browser-bridge"]}`) and names our own binary and a subcommand
/// that no longer exists, so matching that exact shape is itself the proof — and leaving it would
/// have the agent's MCP client fail to start it on every session.
///
/// The per-session proxy entry deliberately does **not** qualify. Its url and bearer were minted
/// per session and never recorded anywhere, so "loopback url plus some `Bearer` header" is a
/// description rather than an identity: it matches any local MCP server an operator configured
/// under this key. An earlier version of this function claimed to recognise "a bearer we minted"
/// while comparing against nothing we had kept. With no way to prove ownership we fail closed and
/// preserve — deleting an operator's configuration is worse than leaving a dead entry, and with
/// the proxy gone the entry it names is dead configuration rather than a live bypass.
fn is_openab_direct_browser_entry(entry: &Value) -> bool {
    entry.get("command").and_then(Value::as_str) == Some("openab")
        && entry.get("args") == Some(&json!(["browser-bridge"]))
}

/// Drop a stale direct-transport `openab-browser` entry from an `mcpServers` map, returning
/// whether anything was removed. Both entries otherwise load side by side and the model may pick
/// the direct one, bypassing the facade's policy and audit.
fn strip_direct_browser_entry(cfg: &mut Value) -> bool {
    let Some(servers) = cfg.get_mut("mcpServers").and_then(Value::as_object_mut) else {
        return false;
    };
    match servers.get("openab-browser") {
        Some(entry) if is_openab_direct_browser_entry(entry) => {
            servers.remove("openab-browser");
            true
        }
        _ => false,
    }
}

/// Write the STATIC, write-once `openab` facade entry into each colocated CLI's MCP config
/// (Facade mode). Like the Option C bridge entry it is byte-identical for every session —
/// the per-session secret is NOT in the file: the entry references the
/// `OPENAB_SESSION_TOKEN` environment variable, which the pool injects into each spawned
/// agent process (config-var expansion is exactly how deployed agents already reference
/// per-bot secrets). No cross-session clobber, nothing to clean up on evict — the token
/// dies with the agent process and its registry entry.
pub async fn write_facade_mcp_config(workdir: &str, facade_url: &str) -> std::io::Result<()> {
    let entry = json!({
        "url": facade_url,
        "headers": { "Authorization": "Bearer ${OPENAB_SESSION_TOKEN}" }
    });
    let cfg_paths = [
        std::path::Path::new(workdir).join(".cursor").join("mcp.json"),
        std::path::Path::new(workdir)
            .join(".kiro")
            .join("settings")
            .join("mcp.json"),
    ];
    for cfg_path in &cfg_paths {
        if let Some(dir) = cfg_path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let mut cfg: Value = match tokio::fs::read(cfg_path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({})),
            Err(_) => json!({}),
        };
        if !cfg.get("mcpServers").map(Value::is_object).unwrap_or(false) {
            cfg["mcpServers"] = json!({});
        }
        // Publish under "openab" (the facade), not "openab-browser": the agent
        // reaches ALL facade capabilities through this one entry.
        let mut changed = false;
        if cfg["mcpServers"]["openab"] != entry {
            cfg["mcpServers"]["openab"] = entry.clone();
            changed = true;
        }
        // Retire the direct transport we previously wrote here. Leaving it means both entries
        // load and the model can reach the browser without passing through facade policy/audit.
        changed |= strip_direct_browser_entry(&mut cfg);
        if changed {
            tokio::fs::write(cfg_path, serde_json::to_vec_pretty(&cfg)?).await?;
        }
    }
    // kiro `--agent` deployments read agent files, not settings/mcp.json.
    merge_kiro_agent_facade_configs(workdir, &entry).await?;
    Ok(())
}

/// Facade-mode sibling of [`merge_kiro_agent_configs`]: merges the static
/// `openab` facade entry + `@openab` allowlist grant into every
/// `.kiro/agents/*.json`. Same never-clobber rules; nothing to clean up on
/// evict (the entry is static and the token lives in the process env).
async fn merge_kiro_agent_facade_configs(workdir: &str, entry: &Value) -> std::io::Result<()> {
    let dir = std::path::Path::new(workdir).join(".kiro").join("agents");
    let Ok(mut rd) = tokio::fs::read_dir(&dir).await else {
        return Ok(());
    };
    while let Ok(Some(f)) = rd.next_entry().await {
        let path = f.path();
        let name = f.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".json") || name.starts_with("._") {
            continue;
        }
        let Ok(bytes) = tokio::fs::read(&path).await else {
            continue;
        };
        let Ok(mut cfg) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if !cfg.get("mcpServers").map(Value::is_object).unwrap_or(false) {
            cfg["mcpServers"] = json!({});
        }
        let mut changed = false;
        if cfg["mcpServers"]["openab"] != *entry {
            cfg["mcpServers"]["openab"] = entry.clone();
            changed = true;
        }
        // Same retirement as the settings files, plus the agent-file allowlist grant that made
        // the direct server callable — `allowedTools` is default-deny, so a leftover
        // `@openab-browser` is what keeps the bypass reachable here.
        if strip_direct_browser_entry(&mut cfg) {
            changed = true;
            if let Some(allowed) = cfg.get_mut("allowedTools").and_then(Value::as_array_mut) {
                allowed.retain(|v| v.as_str() != Some("@openab-browser"));
            }
        }
        if let Some(allowed) = cfg.get_mut("allowedTools").and_then(Value::as_array_mut) {
            if !allowed.iter().any(|v| v.as_str() == Some("@openab")) {
                allowed.push(json!("@openab"));
                changed = true;
            }
        }
        if changed {
            write_private(&path, &serde_json::to_vec_pretty(&cfg)?).await?;
        }
    }
    Ok(())
}

/// Broker-side session credential hook (Facade mode). Implemented by the root
/// (closing over the facade's `SessionTokens` registry — core stays free of
/// the openab-mcp dependency); the pool calls it at session spawn/evict.
pub trait SessionTokenRegistrar: Send + Sync {
    /// Mint a fresh token for `channel_id`; returns the value the pool injects as
    /// `OPENAB_SESSION_TOKEN` in the agent's environment.
    ///
    /// Does **not** replace a token the channel already has — tokens for a channel coexist, so a
    /// respawned or racing session gets its own credential without invalidating one a running
    /// agent is still presenting.
    fn mint(&self, channel_id: &str) -> String;
    /// Revoke one specific token (the session that held it was evicted).
    ///
    /// Deliberately keyed by token, not by channel. Because tokens coexist and session lifetimes
    /// overlap, a replaced session's teardown runs *after* its successor has already minted its
    /// own token for the same channel. Revoking by channel would take the successor's live
    /// credential with it and silently cut the new agent off from the facade; revoking this exact
    /// token is a no-op by then instead (review R1).
    fn revoke(&self, token: &str);
}

/// Report, once at startup, whether browser control is enabled — and that
/// `OPENAB_BROWSER_MODE` no longer selects anything.
///
/// Call this from configuration/startup, **not** from a session path: nothing per-session is
/// decided by it any more, and a warning on that path would repeat for every spawn.
///
/// The variable used to choose between three transports. Two are gone and the third is no longer
/// optional, so any value it holds is inert. Ignoring it silently would leave an operator
/// believing they had configured something — the same failure the removed-bridge warning existed
/// to prevent, one level up. So the message says the value is ignored *and* reports what is
/// actually in force, since "ignored" alone does not tell them whether they still have browser
/// control at all.
pub fn report_browser_control(mcp_configured: bool) {
    if mcp_configured {
        tracing::info!("browser control: enabled via the OAB MCP Facade ([mcp] configured)");
    } else {
        // Unconditional, and the whole point of the change: with the proxy fallback gone, an
        // unconfigured deployment has NO browser control. Saying nothing would leave that to be
        // inferred from tools that never appear — which is the failure this replaced, not a
        // quieter version of it.
        tracing::info!(
            "browser control: unconfigured — no [mcp] section in config.toml, so browser tools \
             are unavailable and nothing was started. Add [mcp] to enable them."
        );
    }
    let raw = std::env::var("OPENAB_BROWSER_MODE").ok();
    if let Some((requested, browser_control)) =
        browser_mode_migration_notice(raw.as_deref(), mcp_configured)
    {
        tracing::warn!(
            requested,
            browser_control,
            "OPENAB_BROWSER_MODE is ignored — it no longer selects a transport and can be unset. \
             Browser control is configured by the [mcp] section of config.toml; `browser_control` \
             reports what is actually in force for this process."
        );
    }
}

/// Decide whether to warn and what to say: `(requested, browser_control)`, or `None` to stay quiet.
///
/// Split out from the logging so the decision is testable without a subscriber or process env.
///
/// **Every** non-empty value warns, `proxy` and `facade` included. `proxy` no longer selects
/// anything either, so staying quiet for it would be the same silence this notice exists to
/// remove; `facade` is merely redundant, but reporting it costs one line at startup and saying
/// "this variable is read" of some values and not others would be false.
fn browser_mode_migration_notice(
    raw: Option<&str>,
    mcp_configured: bool,
) -> Option<(&str, &'static str)> {
    let requested = raw?.trim();
    if requested.is_empty() {
        return None;
    }
    Some((
        requested,
        if mcp_configured { "facade" } else { "disabled" },
    ))
}


#[cfg(test)]
mod tests {
    use super::{
        browser_mode_migration_notice, browser_tools, is_openab_direct_browser_entry,
        write_facade_mcp_config,
    };

    /// The variable is inert now, so the notice must fire for every value an operator could have
    /// set — including `proxy`, which used to be a real selection. Staying quiet for it would
    /// reproduce the silence this notice exists to remove.
    #[test]
    fn every_set_browser_mode_value_is_reported_as_ignored() {
        for v in ["bridge", "proxy", "facade", "  Bridge  ", "typo"] {
            assert!(
                browser_mode_migration_notice(Some(v), true).is_some(),
                "{v:?} is a value someone deliberately set; it must not be ignored silently"
            );
        }
        // Unset and blank express no preference — warning on them would fire for every default
        // deployment and train operators to skip the line.
        assert_eq!(browser_mode_migration_notice(None, true), None);
        assert_eq!(browser_mode_migration_notice(Some(""), true), None);
        assert_eq!(browser_mode_migration_notice(Some("   "), true), None);
    }

    /// The second field is the one an operator actually needs: not which mode they are in (there
    /// are none left) but whether they still have browser control at all. Without `[mcp]` they do
    /// not, and the removed Facade->Proxy fallback no longer hides that.
    #[test]
    fn the_notice_reports_whether_browser_control_survives_not_which_mode() {
        assert_eq!(
            browser_mode_migration_notice(Some("bridge"), true),
            Some(("bridge", "facade"))
        );
        assert_eq!(
            browser_mode_migration_notice(Some("bridge"), false),
            Some(("bridge", "disabled"))
        );
        // Trimmed, so the log shows what was set rather than the surrounding whitespace.
        assert_eq!(
            browser_mode_migration_notice(Some("  proxy  "), false),
            Some(("proxy", "disabled"))
        );
    }

    // --- F4: facade setup retires the direct transport it replaces ---

    /// The bridge and per-session-proxy entries we wrote are recognised; anything else under the
    /// same key is not ours to delete.
    #[test]
    fn only_our_own_direct_browser_shapes_are_recognised() {
        let bridge = serde_json::json!({ "command": "openab", "args": ["browser-bridge"] });
        assert!(is_openab_direct_browser_entry(&bridge));

        // Not provably ours. The loopback+bearer shapes are the important ones: they describe our
        // old proxy entry, but they equally describe an operator's own local MCP server, and the
        // per-session url/bearer were never recorded, so ownership cannot be established.
        for foreign in [
            serde_json::json!({ "url": "http://127.0.0.1:45678/mcp", "headers": { "Authorization": "Bearer abc" } }),
            serde_json::json!({ "url": "https://example.com/mcp", "headers": { "Authorization": "Bearer x" } }),
            serde_json::json!({ "url": "http://127.0.0.1:45678/mcp" }),
            serde_json::json!({ "command": "openab", "args": ["something-else"] }),
            serde_json::json!({ "command": "my-browser-tool", "args": ["browser-bridge"] }),
            serde_json::json!({ "url": "http://127.0.0.1:/mcp", "headers": { "Authorization": "Bearer x" } }),
        ] {
            assert!(
                !is_openab_direct_browser_entry(&foreign),
                "must not claim ownership of {foreign}"
            );
        }
    }

    #[tokio::test]
    async fn facade_setup_removes_the_stale_direct_entry_but_keeps_user_servers() {
        let dir = tempfile::tempdir().unwrap();
        let cursor = dir.path().join(".cursor");
        std::fs::create_dir_all(&cursor).unwrap();
        std::fs::write(
            cursor.join("mcp.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "mcpServers": {
                    // ours, the bridge transport facade mode replaces
                    "openab-browser": { "command": "openab", "args": ["browser-bridge"] },
                    // the operator's own servers must survive untouched
                    "github": { "url": "http://ghpool:8080/mcp" },
                    "notes": { "command": "notes-mcp", "args": ["--stdio"] }
                },
                "someUnrelatedKey": 42
            }))
            .unwrap(),
        )
        .unwrap();

        write_facade_mcp_config(dir.path().to_str().unwrap(), "http://127.0.0.1:8848/mcp")
            .await
            .unwrap();

        let cfg: serde_json::Value =
            serde_json::from_slice(&std::fs::read(cursor.join("mcp.json")).unwrap()).unwrap();
        let servers = cfg["mcpServers"].as_object().unwrap();
        assert!(
            !servers.contains_key("openab-browser"),
            "the direct transport must not load alongside the facade — that is the bypass"
        );
        assert_eq!(servers["openab"]["url"], "http://127.0.0.1:8848/mcp");
        assert_eq!(servers["github"]["url"], "http://ghpool:8080/mcp");
        assert_eq!(servers["notes"]["command"], "notes-mcp");
        assert_eq!(cfg["someUnrelatedKey"], 42, "unrelated config must survive");
    }

    /// An operator's own local MCP server under this key survives facade setup — the entry **and**
    /// its allowlist grant (review R3-F2).
    ///
    /// The previous matcher treated any loopback url carrying any `Bearer` header as ours, which
    /// is precisely the shape a locally-run MCP server takes, so that configuration was deleted.
    /// Ownership of that shape cannot be proven — the per-session url and bearer were never
    /// recorded — so it is preserved now.
    #[tokio::test]
    async fn a_local_mcp_server_under_our_key_is_not_deleted() {
        let wd = tmp_workdir("r3f2").await;
        let cursor = wd.join(".cursor");
        tokio::fs::create_dir_all(&cursor).await.unwrap();
        // Indistinguishable from our retired proxy entry by shape alone.
        let theirs = serde_json::json!({
            "url": "http://127.0.0.1:45678/mcp",
            "headers": { "Authorization": "Bearer their-own-token" }
        });
        tokio::fs::write(
            cursor.join("mcp.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({ "mcpServers": { "openab-browser": theirs } }),
            )
            .unwrap(),
        )
        .await
        .unwrap();

        let agent = wd.join(".kiro/agents/terra.json");
        tokio::fs::write(
            &agent,
            serde_json::to_vec_pretty(&serde_json::json!({
                "name": "terra",
                "mcpServers": { "openab-browser": theirs },
                "allowedTools": ["@builtin", "@openab-browser"]
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        write_facade_mcp_config(wd.to_str().unwrap(), "http://127.0.0.1:8848/mcp")
            .await
            .unwrap();

        let cfg: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(cursor.join("mcp.json")).await.unwrap())
                .unwrap();
        assert_eq!(
            cfg["mcpServers"]["openab-browser"], theirs,
            "an entry we cannot prove we wrote must be preserved verbatim"
        );

        let agent_cfg: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&agent).await.unwrap()).unwrap();
        assert_eq!(agent_cfg["mcpServers"]["openab-browser"], theirs);
        let allowed: Vec<&str> = agent_cfg["allowedTools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            allowed.contains(&"@openab-browser"),
            "the grant must survive too — revoking it silently disables the operator's own server"
        );
        let _ = tokio::fs::remove_dir_all(&wd).await;
    }

    #[tokio::test]
    async fn facade_setup_leaves_a_foreign_openab_browser_entry_alone() {
        // Same key, but a shape we never wrote: it belongs to the operator, so removing it would
        // destroy their configuration to fix a bypass that entry does not create.
        let dir = tempfile::tempdir().unwrap();
        let cursor = dir.path().join(".cursor");
        std::fs::create_dir_all(&cursor).unwrap();
        let foreign = serde_json::json!({ "url": "https://my-own-browser.example/mcp" });
        std::fs::write(
            cursor.join("mcp.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "mcpServers": { "openab-browser": foreign }
            }))
            .unwrap(),
        )
        .unwrap();

        write_facade_mcp_config(dir.path().to_str().unwrap(), "http://127.0.0.1:8848/mcp")
            .await
            .unwrap();

        let cfg: serde_json::Value =
            serde_json::from_slice(&std::fs::read(cursor.join("mcp.json")).unwrap()).unwrap();
        assert_eq!(
            cfg["mcpServers"]["openab-browser"], foreign,
            "an entry we did not write must be preserved verbatim"
        );
    }

    #[tokio::test]
    async fn facade_setup_retires_the_direct_entry_and_its_grant_in_kiro_agent_files() {
        let wd = tmp_workdir("f4-agent").await;
        let agent = wd.join(".kiro/agents/terra.json");
        tokio::fs::write(
            &agent,
            serde_json::to_vec_pretty(&serde_json::json!({
                "name": "terra",
                "mcpServers": {
                    "openab-browser": { "command": "openab", "args": ["browser-bridge"] },
                    "github": { "url": "http://ghpool:8080/mcp" }
                },
                "allowedTools": ["@builtin", "@openab-browser", "@github"]
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        write_facade_mcp_config(wd.to_str().unwrap(), "http://127.0.0.1:8848/mcp")
            .await
            .unwrap();

        let cfg: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&agent).await.unwrap()).unwrap();
        assert!(!cfg["mcpServers"].as_object().unwrap().contains_key("openab-browser"));
        assert_eq!(cfg["mcpServers"]["github"]["url"], "http://ghpool:8080/mcp");
        let allowed: Vec<&str> = cfg["allowedTools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            !allowed.contains(&"@openab-browser"),
            "allowedTools is default-deny — a leftover grant is what keeps the bypass reachable"
        );
        assert!(allowed.contains(&"@openab"), "the facade must be granted");
        assert!(allowed.contains(&"@github"), "unrelated grants must survive");
        let _ = tokio::fs::remove_dir_all(&wd).await;
    }

    /// Unique throwaway workdir with a `.kiro/agents/` tree.
    async fn tmp_workdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "oab-mcp-proxy-test-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        tokio::fs::create_dir_all(dir.join(".kiro").join("agents"))
            .await
            .unwrap();
        dir
    }











    // --- Option C: browser-bridge socket dispatch ---


    #[test]
    fn browser_tools_advertises_the_fixed_set() {
        let tools = browser_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(
            names,
            [
                "katashiro.click",
                "katashiro.read_dom",
                "katashiro.navigate",
                "katashiro.type",
                "katashiro.screenshot"
            ]
        );
    }

    #[test]
    fn every_browser_tool_has_an_object_input_schema() {
        for t in browser_tools() {
            assert_eq!(
                t.input_schema.get("type").and_then(|v| v.as_str()),
                Some("object"),
                "tool {} must have an object input schema",
                t.name
            );
            assert!(t.description.is_some(), "tool {} needs a description", t.name);
        }
    }

    const INIT_BODY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;


}

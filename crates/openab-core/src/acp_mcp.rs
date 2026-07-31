//! Agent-facing wiring for MCP-over-ACP browser control (feature `acp-mcp`).
//!
//! The module name is historical. It no longer hosts a proxy: the per-session loopback MCP
//! server, its bearer and its per-session config rewrite were removed along with the stdio
//! bridge, leaving the OAB MCP Facade as the only way an agent reaches browser tools.
//!
//! What remains is the seam between core and the colocated agent CLI:
//!
//! - [`report_facade_status`] — startup report of whether browser control is on.

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
    /// registry, P1). An empty `server_id` is a sentinel meaning "the sole tunnel on this
    /// channel". This used to say the "proxy/bridge callers" do not yet know the client-declared
    /// id at spawn time; those callers were removed with the proxy and the bridge, so the sentinel
    /// now exists for the facade source's own single-server path (real per-server routing lands
    /// in P2).
    async fn call(
        &self,
        channel_id: &str,
        server_id: &str,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, String>;


    /// Resolve a declared server NAME to the `server_id` the registry keys that tunnel by, for one
    /// channel.
    ///
    /// The only way to reach a tunnel by name. There was also an enumerating `servers()`, and both
    /// its callers collapsed `name -> id` themselves: routing took the first match, discovery took
    /// whichever a `HashMap` kept last. Two collapse rules for one fact, neither beside the eviction
    /// that makes the fact true. Both now call this, and the enumerator is gone rather than left as a
    /// second route someone would reasonably mistake for the supported one.
    ///
    /// Required, with no default. A default returning `None` compiles for every existing implementor
    /// and then silently answers "not connected" for all routing — the failure surfaces at run time,
    /// in tests belonging to whoever did NOT add the method. A missing implementation should be a
    /// compile error, not a behaviour change. This was not hypothetical: adding it with a `None`
    /// default broke five routing tests that had nothing to do with the change.
    fn resolve_by_name(&self, channel_id: &str, server_name: &str) -> Option<String>;
}





/// Write `value` to `path` atomically, owner-only.
///
/// Same-directory temp file created `0600` *before* any bytes reach it, then `rename`. Writing in
/// place leaves a window where a reader sees a half-written config, and chmod-after-write leaves
/// one where the file is world-readable. `rename` within a directory is atomic, so a concurrent
/// reader sees either the old file or the new one.
async fn write_json_atomic(path: &std::path::Path, value: &Value) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    // Unique per WRITE, not per process. A fixed name made concurrent writers share one temp file:
    // both opened it with `truncate`, both wrote, and whichever renamed first left the other
    // renaming a path that no longer existed. A pid alone does not fix that — the racing writers
    // are usually two tasks in the SAME process — so the counter is what makes each attempt
    // distinct, and the pid keeps a respawn that overlaps its predecessor off the same paths.
    //
    // This separates two concerns that were tangled: the lock orders read-modify-write so a writer
    // cannot publish over a state it never read, and the unique temp name stops writers colliding
    // on the intermediate file. Previously the lock was doing both, which is why removing it
    // failed with ENOENT — a filename collision reported as if it were a lost update.
    static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = dir.join(format!(
        ".{}.openab-tmp.{}.{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("mcp.json"),
        std::process::id(),
        TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    {
        let mut opts = tokio::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            // tokio's OpenOptions carries `mode` itself; no std extension trait needed.
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).await?;
        use tokio::io::AsyncWriteExt;
        f.write_all(&bytes).await?;
        f.flush().await?;
        // Durability before the rename: a crash must not leave the new name pointing at a file
        // whose contents never reached disk.
        f.sync_all().await?;
    }
    match tokio::fs::rename(&tmp, path).await {
        Ok(()) => {
            // `sync_all` above made the CONTENTS durable; the rename that publishes them is a
            // directory operation and is not covered by it. Without this a crash can leave the
            // directory entry unwritten while the data it points at is safely on disk — "fsynced,
            // then renamed" is not the same as "the rename survived".
            // Report rather than swallow: this function's whole purpose is durability, so a
            // silent failure of the step that provides it is the worst shape available. Not fatal
            // — the data is written and visible, only the rename's survival across a crash is
            // unproven. On Windows a directory cannot be opened as a file at all, so this is
            // expected to fail there and is logged at debug rather than warn for that reason.
            match tokio::fs::File::open(dir).await {
                Ok(d) => {
                    if let Err(e) = d.sync_all().await {
                        // `warn!`, not `debug!`: opening a directory can legitimately fail (Windows),
                        // but an fsync that runs and FAILS is an unexpected durability failure, and
                        // reporting it more quietly than the thing it protects would hide the worse
                        // of the two — the same reasoning as the failed-handshake disconnect.
                        tracing::warn!(dir = %dir.display(), error = %e,
                            "MCP config: directory fsync failed — the rename may not survive a crash");
                    }
                }
                Err(e) => tracing::debug!(dir = %dir.display(), error = %e,
                    "MCP config: could not open the directory to fsync it (expected on Windows)"),
            }
            Ok(())
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(e)
        }
    }
}




/// Author the static `openab` facade entry into the one file openab owns
/// (Facade mode). The entry is byte-identical for every session — the bridge it used to be
/// compared against here was removed with the rest of Option C, so the comparison is gone
/// rather than left pointing at code that no longer exists.
///
/// The per-session secret is NOT in the file: the entry references the
/// `OPENAB_SESSION_TOKEN` environment variable, which the pool injects into each spawned
/// agent process (config-var expansion is exactly how deployed agents already reference
/// per-bot secrets). No cross-session clobber, nothing to clean up on evict — the token
/// dies with the agent process and its registry entry.
pub async fn write_facade_mcp_config(workdir: &str, facade_url: &str) -> std::io::Result<()> {
    let cfg = json!({
        "mcpServers": {
            // Published as "openab" (the facade), not "openab-browser": the agent reaches ALL
            // facade capabilities through this one entry.
            "openab": {
                "url": facade_url,
                "headers": { "Authorization": "Bearer ${OPENAB_SESSION_TOKEN}" }
            }
        }
    });
    let path = facade_config_path(workdir);
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    // No read-modify-write and no per-path lock. The content is a pure function of `facade_url`,
    // so concurrent sessions write identical bytes and the rename in `write_json_atomic` makes
    // last-one-wins harmless: there is no prior state to lose, because we own the file.
    write_json_atomic(&path, &cfg).await
}

/// The one file openab authors: `<workdir>/.openab/mcp-facade.json`.
///
/// Source for the operator's `kiro-cli mcp import --file … workspace`, and the intended source for
/// Claude Code's `--mcp-config`, which the OPERATOR adds to `[agent] args` — openab does not
/// pass it and identifies no vendor at spawn time (D-21). openab never puts the entry in
/// place itself either (D-15).
pub fn facade_config_path(workdir: &str) -> std::path::PathBuf {
    std::path::Path::new(workdir).join(".openab").join("mcp-facade.json")
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

/// Report, once at startup, whether browser control is enabled.
///
/// Call this from configuration/startup, **not** from a session path: nothing per-session is
/// decided by it, and a warning on that path would repeat for every spawn.
///
/// There used to be a migration notice here for `OPENAB_BROWSER_MODE`. It was removed on
/// 2026-07-31 (D-23): that variable was introduced and retired entirely within this changeset and
/// never appeared in any release, so the notice told operators that something they never had is
/// now ignored.
pub fn report_facade_status(mcp_configured: bool, workdir: &str) {
    if mcp_configured {
        // "enabled" alone became false when openab stopped wiring vendor configs (D-15). The
        // facade IS running, but no agent can reach it until the entry is placed, and an operator
        // reading "enabled" would go looking for a bug instead of doing the remaining step. So the
        // line reports the facade AND names the step, with the exact commands.
        //
        // `workdir` here is the CONFIGURED default. A session may resolve a different one
        // (`effective_workdir`: a stored per-session value, or an explicit override), and the file
        // is written under whichever that session used. Startup cannot know those, so the path
        // below is the default rather than a promise about every session — which is also why the
        // deployed default matters: with `working_dir == $HOME` the two coincide.
        let path = facade_config_path(workdir);
        tracing::info!(
            facade_config = %path.display(),
            "browser control: the OAB MCP Facade is running ([mcp] configured), and openab has \
             written its entry to the file above. openab does NOT modify your agent's MCP config, \
             so browser tools stay unavailable until that entry is in place."
        );
        tracing::info!(
            "browser control — to finish wiring, run ONE of these for your agent:  \
             kiro:  kiro-cli mcp import --file {path} workspace   (do not pass --force)  |  \
             cursor: no import mechanism exists — paste the contents of {path} into the \
             \"mcpServers\" object of .cursor/mcp.json yourself",
            path = path.display()
        );
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
}



/// The destructive cases for the only code that touches a user's `mcp.json`.
///
/// Each of these previously either destroyed a file or panicked the process, and none of them is
/// exotic: a comment in a JSON config is common, `[]` is what an empty array-shaped config looks
/// like, and two sessions starting together is the normal case on a busy pod.
#[cfg(test)]
mod facade_config_writer {
    use super::*;

    async fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("openab-cfg-{tag}-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&d).await;
        tokio::fs::create_dir_all(&d).await.unwrap();
        d
    }

    /// We author exactly one file, in our own directory, with the facade entry.
    #[tokio::test]
    async fn the_facade_entry_is_written_to_the_file_we_own() {
        let wd = tmp_dir("authored").await;
        write_facade_mcp_config(wd.to_str().unwrap(), "http://127.0.0.1:8848/mcp")
            .await
            .unwrap();

        let path = facade_config_path(wd.to_str().unwrap());
        assert_eq!(path, wd.join(".openab").join("mcp-facade.json"));
        let v: Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["openab"]["url"], json!("http://127.0.0.1:8848/mcp"));
        // The token is never in the file — the literal is, and the agent's env supplies the value.
        assert_eq!(
            v["mcpServers"]["openab"]["headers"]["Authorization"],
            json!("Bearer ${OPENAB_SESSION_TOKEN}")
        );
        let _ = tokio::fs::remove_dir_all(&wd).await;
    }

    /// THE BOUNDARY (D-2026-07-30-03, D-2026-07-30-15): openab never modifies a file it did not
    /// create, and never creates one in someone else's directory.
    ///
    /// Every defect in canonical item 9 — EACCES read as "file absent" then overwritten, no
    /// directory fsync, rename dropping mode/owner and replacing symlinks, a concurrency test that
    /// passed for the wrong reason — was a consequence of editing the operator's `mcp.json`.
    /// Deleting that path deleted all four, and this test is what stops it coming back: a
    /// reintroduced merge would have to modify one of these files to be useful, and that fails
    /// here rather than in someone's deployment.
    #[tokio::test]
    async fn an_operators_own_mcp_config_is_never_touched() {
        let wd = tmp_dir("boundary").await;
        let cursor = wd.join(".cursor").join("mcp.json");
        let kiro = wd.join(".kiro").join("settings").join("mcp.json");
        let agent = wd.join(".kiro").join("agents").join("terra.json");
        for f in [&cursor, &kiro, &agent] {
            tokio::fs::create_dir_all(f.parent().unwrap()).await.unwrap();
        }
        // Deliberately including a `//` comment: the shape that the old merge path destroyed.
        let cursor_body = "{\n  // mine\n  \"mcpServers\": {\"mine\": {\"command\": \"x\"}}\n}";
        let kiro_body = "{\"mcpServers\":{\"openab-browser\":{\"command\":\"openab\",\"args\":[\"browser-bridge\"]}}}";
        let agent_body = "{\"allowedTools\":[\"@openab-browser\",\"@github\"]}";
        tokio::fs::write(&cursor, cursor_body).await.unwrap();
        tokio::fs::write(&kiro, kiro_body).await.unwrap();
        tokio::fs::write(&agent, agent_body).await.unwrap();

        write_facade_mcp_config(wd.to_str().unwrap(), "http://127.0.0.1:8848/mcp")
            .await
            .unwrap();

        // Byte-for-byte, including the stale `openab-browser` entry and its grant. openab no
        // longer retires those — see the PR body: that cleanup became operator-performed, and it
        // is a policy bypass rather than a convenience, so it is stated rather than silently
        // dropped.
        assert_eq!(tokio::fs::read_to_string(&cursor).await.unwrap(), cursor_body);
        assert_eq!(tokio::fs::read_to_string(&kiro).await.unwrap(), kiro_body);
        assert_eq!(tokio::fs::read_to_string(&agent).await.unwrap(), agent_body);
        let _ = tokio::fs::remove_dir_all(&wd).await;
    }

    /// A vendor directory that does not exist must not be created either — absence of a file is
    /// not permission to author one.
    #[tokio::test]
    async fn no_vendor_directory_is_created() {
        let wd = tmp_dir("novendor").await;
        write_facade_mcp_config(wd.to_str().unwrap(), "http://127.0.0.1:8848/mcp")
            .await
            .unwrap();
        for d in [".cursor", ".kiro"] {
            assert!(
                !wd.join(d).exists(),
                "{d} was created — openab may only author inside .openab/"
            );
        }
        let _ = tokio::fs::remove_dir_all(&wd).await;
    }

    /// Concurrent sessions must still publish a readable file.
    ///
    /// Weaker than the merge-path version by design: with no read-modify-write there is no lost
    /// update to prevent, because both writers produce identical bytes from the same `facade_url`.
    /// What remains worth pinning is that the rename never exposes a partial file.
    #[tokio::test]
    async fn concurrent_writers_publish_a_readable_config() {
        let wd = tmp_dir("concurrent").await;
        let w = wd.to_str().unwrap().to_string();
        let (a, b) = tokio::join!(
            write_facade_mcp_config(&w, "http://127.0.0.1:8848/mcp"),
            write_facade_mcp_config(&w, "http://127.0.0.1:8848/mcp"),
        );
        a.unwrap();
        b.unwrap();
        let v: Value =
            serde_json::from_slice(&tokio::fs::read(facade_config_path(&w)).await.unwrap())
                .unwrap();
        assert_eq!(v["mcpServers"]["openab"]["url"], json!("http://127.0.0.1:8848/mcp"));
        let _ = tokio::fs::remove_dir_all(&wd).await;
    }

    /// The file we write is owner-only.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_written_config_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let wd = tmp_dir("perms").await;
        write_facade_mcp_config(wd.to_str().unwrap(), "http://127.0.0.1:8848/mcp")
            .await
            .unwrap();
        let path = facade_config_path(wd.to_str().unwrap());
        let mode = tokio::fs::metadata(&path).await.unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "0600 must be set at creation, not chmod'd after");
        let _ = tokio::fs::remove_dir_all(&wd).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{
    };









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













    const INIT_BODY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;


}

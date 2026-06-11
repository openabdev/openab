//! Native MCP client. See `docs/adr/openab-agent-mcp.md`.

pub mod breaker;
pub mod config;
pub mod flow;
pub mod meta_tool;
pub mod oauth;
pub mod runtime;
pub mod sampling;

use serde_json::json;

use crate::auth::{auth_path, McpCredentialStore};
use crate::llm::ToolDef;
use config::{McpConfig, ServerConfig};

pub use runtime::McpRuntimeManager;

/// Secret-key tokens (lowercased) whose following value [`redact_secrets`]
/// masks. Conservative, always-on built-in set — the env/`redact.toml`
/// configurable variant from the spec note (Section 17 §4) is deferred as
/// YAGNI; a fixed list covers the realistic leak vectors without a config
/// surface or a new dependency.
const REDACT_KEYS: &[&str] = &[
    "authorization",
    "access_token",
    "refresh_token",
    "client_secret",
    "api_key",
    "apikey",
    "api-key",
    "password",
    "passwd",
    "secret",
    "token",
    "bearer",
];

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// AWS access key id: `AKIA` + 16 upper-alnum, not embedded in a longer token.
fn is_akia(bytes: &[u8], i: usize) -> bool {
    if i + 20 > bytes.len() || &bytes[i..i + 4] != b"AKIA" {
        return false;
    }
    if i > 0 && is_word_byte(bytes[i - 1]) {
        return false;
    }
    if !bytes[i + 4..i + 20]
        .iter()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return false;
    }
    !(i + 20 < bytes.len()
        && (bytes[i + 20].is_ascii_uppercase() || bytes[i + 20].is_ascii_digit()))
}

/// If a [`REDACT_KEYS`] entry starts at `i` (on a word boundary and not part of
/// a longer identifier), return its byte length + canonical form.
fn match_key(lower: &[u8], i: usize) -> Option<(usize, &'static str)> {
    for &k in REDACT_KEYS {
        let kb = k.as_bytes();
        let end = i + kb.len();
        if end <= lower.len() && &lower[i..end] == kb {
            if end < lower.len() && is_word_byte(lower[end]) {
                continue; // e.g. "tokenizer" must not match "token"
            }
            return Some((kb.len(), k));
        }
    }
    None
}

/// Mask secret-like values in a string before it is emitted on one of *our own*
/// tracing / audit / error surfaces (Section 17 §4 / row 624). We can only
/// enforce this on text we author — inbound server log payloads remain the
/// server's authorship obligation (rows 590 / 592-594), so this pairs with
/// (it does not replace) that server-side duty.
///
/// Masks: (a) the value after a known secret key + `=`/`:` separator (handling
/// a quoted JSON key like `"token": "v"`), with `authorization`/`bearer` values
/// running to end-of-field so `Bearer <jwt>` is fully hidden; (b) AWS access key
/// IDs. Conservative: it only rewrites runs anchored to a secret keyword, so
/// ordinary diagnostic text passes through unchanged.
pub fn redact_secrets(input: &str) -> String {
    let bytes = input.as_bytes();
    let lower = input.to_ascii_lowercase();
    let lb = lower.as_bytes();
    let n = bytes.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    while i < n {
        if is_akia(bytes, i) {
            out.push_str("AKIA");
            out.push_str(&"*".repeat(16));
            i += 20;
            continue;
        }
        if let Some((klen, key)) = match_key(lb, i) {
            let at_boundary = i == 0 || !is_word_byte(bytes[i - 1]);
            if at_boundary {
                // optional closing quote of a quoted key, then spaces, then sep
                let mut p = i + klen;
                if p < n && (bytes[p] == b'"' || bytes[p] == b'\'') {
                    p += 1;
                }
                while p < n && (bytes[p] == b' ' || bytes[p] == b'\t') {
                    p += 1;
                }
                if p < n && (bytes[p] == b'=' || bytes[p] == b':') {
                    out.push_str(&input[i..=p]); // key + closing-quote/spaces + separator
                    let mut j = p + 1;
                    while j < n && (bytes[j] == b' ' || bytes[j] == b'\t') {
                        out.push(' ');
                        j += 1;
                    }
                    let quote = if j < n && (bytes[j] == b'"' || bytes[j] == b'\'') {
                        let q = bytes[j];
                        out.push(q as char);
                        j += 1;
                        Some(q)
                    } else {
                        None
                    };
                    let run_to_eof = matches!(key, "authorization" | "bearer");
                    let start = j;
                    while j < n {
                        let b = bytes[j];
                        let stop = match quote {
                            Some(q) => b == q,
                            None if run_to_eof => {
                                matches!(b, b',' | b'\n' | b'\r' | b'"' | b'\'' | b'}')
                            }
                            None => matches!(
                                b,
                                b' ' | b'\t'
                                    | b','
                                    | b'&'
                                    | b';'
                                    | b'\n'
                                    | b'\r'
                                    | b'}'
                                    | b')'
                                    | b'"'
                                    | b'\''
                            ),
                        };
                        if stop {
                            break;
                        }
                        j += 1;
                    }
                    if j > start {
                        out.push_str("***");
                    }
                    i = j;
                    continue;
                }
            }
        }
        let ch = input[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Brief, redacted error string for surfaces the caller / LLM sees (row 37b).
/// Uses anyhow's outermost context line (`{err}`) — NOT the full `{err:#}`
/// chain, which stays in `tracing` for operators — then runs [`redact_secrets`]
/// so a credential that leaked into the message text never reaches the model.
pub fn concise_error_message(err: &anyhow::Error) -> String {
    redact_secrets(&err.to_string())
}

/// Shared tool name used by `mcp_tool_def()` and the agent dispatch arm —
/// keeps the implicit contract between the two call sites explicit.
pub const MCP_TOOL_NAME: &str = "mcp";

/// The single `mcp` tool definition the LLM sees (ADR §5.2). The schema is
/// intentionally permissive on the per-action fields — the LLM should call
/// `mcp(action="help")` first to learn the action-specific contract.
pub fn mcp_tool_def() -> ToolDef {
    ToolDef {
        name: MCP_TOOL_NAME.to_string(),
        description: "Talk to configured MCP servers. Call with \
             {action: 'help'} first to see the available actions \
             (help, list_servers, list_tools, describe_tool, call, status)."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["help", "list_servers", "list_tools",
                             "describe_tool", "call", "status"],
                    "description": "Which meta-tool action to invoke"
                },
                "server": {
                    "type": "string",
                    "description": "Server name (required by list_tools / describe_tool / call; optional filter for status)"
                },
                "tool": {
                    "type": "string",
                    "description": "Tool name on the server (required by describe_tool / call)"
                },
                "arguments": {
                    "description": "Tool arguments for call — JSON object, or null/omitted for no-arg tools"
                }
            },
            "required": ["action"]
        }),
    }
}

fn load_config_or_exit() -> McpConfig {
    McpConfig::load().unwrap_or_else(|e| {
        eprintln!("failed to load mcp config: {e:#}");
        std::process::exit(1);
    })
}

fn print_no_servers_hint() {
    println!("No MCP servers configured.");
    println!("  global:  ~/.openab/agent/mcp.json");
    println!("  project: ./.openab/agent/mcp.json");
}

/// Construct an `McpRuntimeManager` from on-disk config — returns `None`
/// when no servers are configured so callers can skip the entire MCP path
/// (saves system-prompt tokens + keeps the LLM from hallucinating an empty
/// tool surface). Parse failure falls back to `None` with a `tracing::warn!`.
/// Long-running servers (ACP, future HTTP) call this; CLI subcommands use
/// `load_config_or_exit` instead.
pub fn load_runtime_or_warn() -> Option<McpRuntimeManager> {
    let cfg = McpConfig::load().unwrap_or_else(|e| {
        tracing::warn!("mcp config failed to load, starting with no servers: {e:#}");
        McpConfig::default()
    });
    if cfg.servers.is_empty() {
        None
    } else {
        Some(McpRuntimeManager::from_config(cfg))
    }
}

/// Build the MCP section appended to the system prompt at session start
/// (PR #959 chaodu F1, discovery slice). Mirrors the skills-catalogue
/// pattern: advertise *server names + transports* — not individual tools —
/// so the LLM knows the surface exists and can call
/// `mcp(action="list_tools", server=...)` to discover capabilities on demand.
///
/// Token-budget invariance: the section grows O(server count), not
/// O(server count × tool count). PR #959 F1 PoC measured ≤100 tokens per
/// server-side meta entry under this pattern; flattening per-tool would
/// blow that invariance up.
///
/// Status semantics worth surfacing to the LLM (matches `status_label` in
/// `meta_tool`): `idle` = ready (lazy-connect on first call), not broken.
pub fn format_system_prompt_appendix(manager: &McpRuntimeManager) -> String {
    let catalog = manager.catalog();
    let mut out = String::from(
        "\n\n## MCP tool\n\n\
         Use the `mcp` tool to talk to configured MCP servers. Key actions: \
         `list_tools(server)` discovers a server's tools, \
         `call(server, tool, arguments)` invokes one. Servers auto-connect \
         on first use — `status: \"idle\"` means ready (not broken); \
         `status: \"failed\"` carries the error reason in `last_error`. \
         Call `mcp(action=\"help\")` only if action shapes are unclear.\n\n",
    );
    if catalog.is_empty() {
        out.push_str(
            "No MCP servers are configured. The `mcp` tool will report an \
             empty `list_servers` until one is added.\n",
        );
        return out;
    }
    out.push_str("Configured servers:\n");
    for entry in catalog {
        if entry.requires_oauth {
            out.push_str(&format!(
                "- **{}** ({}, requires `mcp login {}` before first call)\n",
                entry.name, entry.transport, entry.name,
            ));
        } else {
            out.push_str(&format!("- **{}** ({})\n", entry.name, entry.transport));
        }
    }
    out
}

/// `openab-agent mcp list [--resolve]`.
///
/// Default: print configs verbatim (`${env:VAR}` placeholders kept as-is) so
/// `mcp list` is safe to paste into bug reports. `--resolve` opts into
/// substituting env vars and prints a leading warning — useful for debugging
/// missing-env startup failures locally.
pub fn cli_list_servers(resolve: bool) {
    let cfg = load_config_or_exit();
    if cfg.servers.is_empty() {
        print_no_servers_hint();
        return;
    }
    if resolve {
        eprintln!("⚠ --resolve: env vars substituted into output below.");
        eprintln!("⚠ Output may contain secrets — do not paste publicly.");
        eprintln!();
    }
    let mut servers: Vec<_> = cfg.servers.iter().collect();
    servers.sort_by_key(|(name, _)| *name);
    for (name, server) in servers {
        print_server(name, server, resolve);
    }
    if !resolve {
        // Hint to stderr (keeps stdout paste-clean): values are verbatim, so
        // `${env:..}` placeholders are shown unexpanded unless --resolve is used.
        eprintln!();
        eprintln!("Note: values are shown verbatim; ${{env:VAR}} placeholders are not expanded.");
        eprintln!("      Re-run `mcp list --resolve` to substitute environment variables.");
    }
}

fn print_server(name: &str, server: &ServerConfig, resolve: bool) {
    if resolve {
        match server.resolved(name) {
            Ok(r) => print_json("✓", name, &r),
            Err(e) => println!("✗ {name}: {e:#}"),
        }
    } else {
        print_json("•", name, server);
    }
}

fn print_json<T: serde::Serialize>(status: &str, name: &str, value: &T) {
    println!("{status} {name}");
    if let Ok(json) = serde_json::to_string_pretty(value) {
        for line in json.lines() {
            println!("    {line}");
        }
    }
}

/// `openab-agent mcp status`.
///
/// Prints per-server runtime status. Servers start `Disconnected` and only
/// advance after `mcp connect <name>` (or, later, lazy dial from the agent
/// path). A server in `NeedsAuth` carries a `(run mcp login <name>)` hint.
/// The paste-back login is now single-invocation (PKCE/CSRF live in rmcp's
/// in-memory `StateStore`), so there are no on-disk pending entries to surface.
pub async fn cli_show_status() {
    let cfg = load_config_or_exit();
    let manager = McpRuntimeManager::from_config(cfg.clone());
    if manager.is_empty().await {
        println!("No MCP servers configured.");
        return;
    }
    let auth = auth_path();
    let statuses = manager.statuses().await;
    for (name, status) in &statuses {
        // A fresh CLI process never dials, so `status` is always `Disconnected`
        // (idle). For HTTP servers with an `oauth:` block, peek the same rmcp
        // credential store `connect()`/`doctor` read so the line reflects
        // whether a login is still owed rather than a uniform `○`.
        if let Some(ServerConfig::Http { oauth: Some(_), .. }) = cfg.servers.get(name) {
            use rmcp::transport::CredentialStore;
            let store = McpCredentialStore::new(auth.clone(), name.clone());
            let cached = match store.load().await {
                Ok(Some(creds)) => Some(runtime::classify_stored_creds(&creds)),
                _ => None,
            };
            match cached {
                Some((true, _has_refresh, near_expiry)) => {
                    let note = if near_expiry {
                        "authed, near expiry"
                    } else {
                        "authed, idle"
                    };
                    println!("○ {name} ({note})");
                }
                _ => println!("◌ {name} (run `mcp login {name}`)"),
            }
            continue;
        }
        let mut line = format!("{} {name}", status.icon());
        if matches!(status, runtime::ServerStatus::NeedsAuth) {
            line.push_str(&format!(" (run `mcp login {name}`)"));
        }
        println!("{line}");
    }
}

/// `openab-agent mcp connect <name>`. Spawns the configured stdio server,
/// runs the rmcp handshake, and reports success or the failure reason.
/// The connection is dropped on process exit — this CLI is a smoke-test
/// for `mcp.json` entries, not a long-running session.
pub async fn cli_connect(name: String) {
    let manager = McpRuntimeManager::from_config(load_config_or_exit());
    match manager.connect(&name).await {
        Ok(()) => println!("● connected: {name}"),
        Err(e) => {
            eprintln!("✗ {name}: {e:#}");
            std::process::exit(1);
        }
    }
}

/// `openab-agent mcp login <name>`. Drives the §6.4 paste-back flow
/// end-to-end in a single invocation:
///
/// 1. `start_paste_login` runs rmcp OAuth discovery and builds the authorize
///    URL; the PKCE verifier + CSRF `state` are held in the manager's
///    in-memory `StateStore`
/// 2. The CLI prints the URL for the user to open in a browser, then blocks
///    on stdin waiting for the post-redirect URL to be pasted back
/// 3. `complete_login` exchanges the auth code (rmcp validates the `state`
///    against the stashed entry), auto-persists the `StoredCredentials`, and
///    leaves the server `Disconnected` and ready for `connect`
///
/// Single-invocation by design: the PKCE/CSRF state lives only in this
/// process, so the URL print and the paste-back must happen in the same run.
/// Errors at any step exit non-zero; re-run the command to start a fresh flow.
pub async fn cli_login(name: String, scopes: Vec<String>) {
    let manager = McpRuntimeManager::from_config(load_config_or_exit());
    let start = match manager.start_paste_login(&name, &scopes).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("✗ {name}: {e:#}");
            std::process::exit(1);
        }
    };
    println!("Open this URL in a browser to authorize:");
    println!();
    println!("    {}", start.authorize_url);
    println!();
    let redirect = match read_redirect_from_stdin() {
        Ok(u) => u,
        Err(e) => {
            eprintln!("✗ failed to read redirect URL: {e}");
            std::process::exit(1);
        }
    };
    if redirect.is_empty() {
        eprintln!("✗ empty redirect URL — aborting");
        std::process::exit(1);
    }
    match manager.complete_login(&name, &redirect).await {
        Ok(()) => println!("● logged in: {name}"),
        Err(e) => {
            eprintln!("✗ login failed: {e:#}");
            std::process::exit(1);
        }
    }
}

/// `openab-agent mcp login <name> --device`. Drives the RFC 8628
/// device-code flow end-to-end:
///
/// 1. `start_device_login` POSTs the §3.1 device authorization request,
///    prints the verification URL + user code, and spawns the §3.4
///    polling task in the background
/// 2. This CLI polls `statuses()` until the server transitions away from
///    `Connecting` — `Disconnected` means the polling task persisted the
///    native `StoredCredentials` (next `connect()` picks it up);
///    `NeedsAuth` means the flow terminally failed (access_denied /
///    expired_token / network)
///
/// Wall-clock timeout = `expires_in` returned by the provider. Polling
/// happens in the runtime-spawned task; this loop only watches status,
/// so the user can `Ctrl-C` the CLI without leaking pending state — the
/// detached task dies with the process and `auth.json` stays clean.
pub async fn cli_login_device(name: String) {
    let manager = McpRuntimeManager::from_config(load_config_or_exit());
    let start = match manager.start_device_login(&name).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("✗ {name}: {e:#}");
            std::process::exit(1);
        }
    };
    println!();
    if let Some(complete) = &start.verification_uri_complete {
        println!("Open this URL in a browser (pre-filled with user code):");
        println!();
        println!("    {complete}");
        println!();
    }
    println!("Or open the verification URL and enter the user code:");
    println!();
    println!("    URL:       {}", start.verification_uri);
    println!("    User code: {}", start.user_code);
    println!();
    println!(
        "Waiting for authorization (timeout: {}s)...",
        start.expires_in
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(start.expires_in);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        if std::time::Instant::now() >= deadline {
            eprintln!("✗ device-flow timed out (no user authorization)");
            std::process::exit(1);
        }
        let statuses = manager.statuses().await;
        let status = statuses.iter().find(|(n, _)| n == &name).map(|(_, s)| s);
        match status {
            Some(runtime::ServerStatus::Disconnected) => {
                println!("● logged in: {name}");
                return;
            }
            Some(runtime::ServerStatus::NeedsAuth) => {
                eprintln!("✗ device-flow failed (run `mcp status` / check logs)");
                std::process::exit(1);
            }
            _ => continue,
        }
    }
}

/// `openab-agent mcp doctor`. Per-server diagnostic that runs a live
/// `connect()` against every configured server and surfaces the result
/// plus a remediation hint when something's broken (ADR §8).
///
/// Per-server checks (run in order, short-circuit on first ✗):
/// 1. **Config resolution** — `${env:VAR}` placeholders resolve against
///    the live process env. Missing vars print the offending name +
///    hint to set it.
/// 2. **OAuth state** (HTTP + `oauth:` only) — cached `TokenStore` in
///    `auth.json`. Missing → `mcp login <name>` hint; expired → noted
///    but not fatal (connect will attempt refresh).
/// 3. **Live connect** — `manager.connect(name).await`. Any error is
///    surfaced verbatim (including the circuit-breaker `retry in {n}s`
///    hint from §5.9 when the breaker is open).
///
/// Exits non-zero if any server fails diagnostic, so CI / scripts can
/// `openab-agent mcp doctor || alert`.
pub async fn cli_doctor() {
    let cfg = load_config_or_exit();
    if cfg.servers.is_empty() {
        print_no_servers_hint();
        return;
    }
    let manager = McpRuntimeManager::from_config(cfg.clone());
    let auth = auth_path();
    let mut servers: Vec<_> = cfg.servers.iter().collect();
    servers.sort_by_key(|(name, _)| *name);
    let mut failed = 0usize;
    for (name, server) in &servers {
        println!();
        println!("● {name}  ({})", server.transport_label());
        if !doctor_server(&manager, &auth, name, server).await {
            failed += 1;
        }
    }
    println!();
    if failed == 0 {
        println!("✓ all {} server(s) healthy", servers.len());
    } else {
        println!(
            "✗ {failed} of {} server(s) failed diagnostic",
            servers.len()
        );
        std::process::exit(1);
    }
}

/// Returns `true` if every check passed for this server, `false` on the
/// first failure (subsequent checks are skipped to keep the report focused
/// on the root cause).
async fn doctor_server(
    manager: &McpRuntimeManager,
    auth: &std::path::Path,
    name: &str,
    server: &ServerConfig,
) -> bool {
    if let Err(e) = server.resolved(name) {
        println!("    ✗ config: {}", redact_secrets(&format!("{e:#}")));
        println!("    → set the missing env var(s) above and re-run");
        return false;
    }
    println!("    ✓ config: env vars resolved");
    if let ServerConfig::Http { oauth: Some(_), .. } = server {
        // Read the same native rmcp `StoredCredentials` store that `connect()`
        // uses (device/paste login persist here). The legacy `TokenStore`
        // reader would miss these and false-report "no token cached".
        use rmcp::transport::CredentialStore;
        let store = McpCredentialStore::new(auth.to_path_buf(), name.to_string());
        let cached = match store.load().await {
            Ok(Some(creds)) => Some(runtime::classify_stored_creds(&creds)),
            _ => None,
        };
        match cached {
            Some((true, _has_refresh, near_expiry)) => {
                if near_expiry {
                    println!("    ⚠ oauth: token near expiry (connect will attempt refresh)");
                } else {
                    println!("    ✓ oauth: valid token cached");
                }
            }
            _ => {
                println!("    ✗ oauth: no token cached");
                println!("    → run `openab-agent mcp login {name}`");
                return false;
            }
        }
    }
    match manager.connect(name).await {
        Ok(()) => {
            println!("    ✓ connect: handshake succeeded");
            true
        }
        Err(e) => {
            println!("    ✗ connect: {}", redact_secrets(&format!("{e:#}")));
            false
        }
    }
}

fn read_redirect_from_stdin() -> std::io::Result<String> {
    use std::io::Write;
    print!("Paste the FULL redirect URL: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::McpConfig;

    fn mgr_from(json: &str) -> McpRuntimeManager {
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        McpRuntimeManager::from_config(cfg)
    }

    #[test]
    fn format_system_prompt_appendix_lists_each_server() {
        let mgr = mgr_from(
            r#"{
                "mcpServers": {
                    "fs": { "type": "stdio", "command": "mcp-server-filesystem" },
                    "weather": { "type": "http", "url": "https://example/mcp" }
                }
            }"#,
        );
        let s = format_system_prompt_appendix(&mgr);
        assert!(s.contains("## MCP tool"));
        assert!(s.contains("Configured servers:"));
        assert!(s.contains("**fs** (stdio)"));
        assert!(s.contains("**weather** (http)"));
        // Status semantics must be advertised so LLM doesn't misread `idle`
        // as a failure (PR #959 F1 PoC observation).
        assert!(s.contains("idle"));
    }

    #[test]
    fn format_system_prompt_appendix_marks_oauth_servers() {
        let mgr = mgr_from(
            r#"{
                "mcpServers": {
                    "linear": {
                        "type": "http",
                        "url": "https://mcp.linear.app/mcp",
                        "oauth": { "provider": "linear", "scopes": ["read"] }
                    }
                }
            }"#,
        );
        let s = format_system_prompt_appendix(&mgr);
        assert!(
            s.contains("requires `mcp login linear`"),
            "OAuth servers must surface the login hint; got:\n{s}"
        );
    }

    #[test]
    fn redact_secrets_masks_common_patterns() {
        assert_eq!(redact_secrets("token=abc123"), "token=***");
        assert_eq!(redact_secrets("api_key: sk-XYZ"), "api_key: ***");
        assert_eq!(
            redact_secrets(r#"{"password":"hunter2"}"#),
            r#"{"password":"***"}"#
        );
        assert_eq!(
            redact_secrets("Authorization: Bearer eyJhbGci.foo"),
            "Authorization: ***"
        );
        assert_eq!(
            redact_secrets("creds AKIAIOSFODNN7EXAMPLE end"),
            "creds AKIA**************** end"
        );
    }

    #[test]
    fn redact_secrets_preserves_ordinary_text() {
        // "token" as a substring of a longer word must not trip the masker.
        assert_eq!(
            redact_secrets("tokenizer ran in 5ms"),
            "tokenizer ran in 5ms"
        );
        assert_eq!(
            redact_secrets("connect failed: timeout after 30s"),
            "connect failed: timeout after 30s"
        );
        // UTF-8 passes through untouched.
        assert_eq!(redact_secrets("連線失敗：逾時"), "連線失敗：逾時");
    }

    #[test]
    fn concise_error_message_is_outermost_and_redacted() {
        let e = anyhow::anyhow!("inner cause").context("token=secret123");
        assert_eq!(concise_error_message(&e), "token=***");
    }

    #[test]
    fn format_system_prompt_appendix_handles_empty_catalog() {
        let mgr = mgr_from(r#"{"mcpServers":{}}"#);
        let s = format_system_prompt_appendix(&mgr);
        assert!(s.contains("## MCP tool"));
        assert!(s.contains("No MCP servers are configured"));
        assert!(!s.contains("Configured servers:"));
    }
}

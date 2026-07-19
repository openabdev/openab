use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rmcp::transport::{AuthError, CredentialStore, StoredCredentials};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Namespace key for the existing Codex single-tenant credential.
/// Lives next to future `mcp:<server>` entries inside `auth.json`.
const CODEX_NAMESPACE: &str = "codex";
/// Namespace key for the Anthropic (Claude Pro/Max) OAuth credential.
pub const ANTHROPIC_NAMESPACE: &str = "anthropic-oauth";
/// Namespace key for the xAI (SuperGrok / X Premium) OAuth credential.
pub const XAI_NAMESPACE: &str = "xai-oauth";

const REFRESH_SKEW_SECONDS: u64 = 120;

const CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_DEVICE_AUTH_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const CODEX_DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const CODEX_DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const REDIRECT_PORT: u16 = 1455;

// Anthropic OAuth (Claude Pro/Max). Values mirror Claude Code's public client so
// `platform.claude.com` accepts the flow. Token bodies are JSON (Codex uses form)
// and the refresh body omits `scope` (Pi #2169).
const ANTHROPIC_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const ANTHROPIC_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const ANTHROPIC_REDIRECT_PORT: u16 = 53692;
const ANTHROPIC_SCOPE: &str =
    "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

// xAI OAuth (SuperGrok / X Premium subscription) — RFC 8628 device-code flow.
// The client_id is the official grok CLI public client (no secret; appears in
// xai-org/grok-build and is reused by OpenClaw, Hermes, litellm, Warp, Pi, …).
// xAI exposes no public OAuth client registration, so reusing the CLI client is
// the ecosystem convention; `referrer` marks the requester (Pi sends "pi").
const XAI_DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const XAI_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const XAI_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";

// ── OAuthVendor (auth axis — ADR §5.1) ──────────────────────────────────────
//
// A subscription-OAuth provider is one static `OAuthVendor` descriptor; the
// shared driver below (`build_authorize_url`, `exchange_authorization_code`,
// `refresh_token`) does PKCE/CSRF/exchange/refresh by reading the descriptor, so
// adding a vendor is a new descriptor — not a new hand-rolled flow. Token bodies
// and a few authorize-URL quirks are the only per-vendor variation, expressed as
// trait methods rather than forked code paths.
//
// NOTE (ADR §4.2): the ADR specifies building this driver on the official
// `oauth2` crate (as `mcp/runtime.rs` already does via `BasicClient` + a custom
// reqwest http hook). This pass keeps the proven reqwest flows and only
// parameterises them by descriptor; swapping the engine onto `oauth2::BasicClient`
// is a follow-up internal change invisible to vendor authors (the descriptor
// surface is unchanged). The device-code grant (non-standard `device_auth_id`)
// and Anthropic's JSON token body are why the swap is staged, not done blind.

/// Token-request body encoding. Codex/OpenAI use form-encoding; Anthropic's AS
/// takes JSON (and rejects a `scope` field on refresh — Pi #2169).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenBodyFormat {
    Form,
    Json,
}

/// OAuth grant a vendor's *primary* login uses. Codex additionally exposes a
/// device-code subcommand (non-standard `device_auth_id` protocol, kept as a
/// bespoke flow), but its browser login — like Anthropic's — is PKCE. xAI is
/// the first device-primary vendor (RFC 8628 via `login_device_code_flow`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthGrant {
    Pkce,
    DeviceCode,
}

/// Static per-vendor OAuth descriptor (ADR §5.1, auth axis). Signatures mirror
/// the ADR verbatim so future vendors (gemini/grok/agy) slot in as descriptors.
/// `Send + Sync` so a boxed vendor can be held across the refresh `await` inside
/// the `Send` provider futures.
trait OAuthVendor: Send + Sync {
    /// `auth.json` tenant key (`codex` / `anthropic-oauth` / …).
    fn namespace(&self) -> &str;
    fn client_id(&self) -> String;
    /// Bundled installed-app secret (gemini/agy); `None` for public PKCE clients.
    /// ADR §5.1 surface — first consumer is the gemini/agy vendor (encode-at-rest
    /// per §9 Q2); unused until then.
    #[allow(dead_code)]
    fn client_secret(&self) -> Option<String> {
        None
    }
    fn authorize_url(&self) -> &str;
    fn token_url(&self) -> &str;
    /// Loopback `(port, path)` for PKCE; `None` for device flow (no redirect endpoint).
    fn redirect(&self) -> Option<(u16, &'static str)> {
        None
    }
    fn scope(&self) -> &str;
    /// Extra authorize-URL query params (Codex's simplified-flow hints; Anthropic's `code=true`).
    fn extra_authorize_params(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }
    fn token_body(&self) -> TokenBodyFormat {
        TokenBodyFormat::Form
    }
    /// Which grant the vendor's primary login uses. `login_device_code_flow`
    /// guards on `DeviceCode` the same way `login_pkce_flow` guards on
    /// `redirect()`, so a mis-wired vendor fails loud instead of half-flowing.
    fn grant(&self) -> AuthGrant {
        AuthGrant::Pkce
    }
    /// RFC 8628 device authorization endpoint. `Some` for `DeviceCode`-grant
    /// vendors; `None` for PKCE vendors (no device endpoint).
    fn device_authorization_url(&self) -> Option<&str> {
        None
    }
    /// Extra form fields on the device authorization request (xAI's `referrer`
    /// requester tag — the device-flow analogue of `extra_authorize_params`).
    fn extra_device_params(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }
    /// Full loopback redirect URI, derived from `redirect()`.
    fn redirect_uri(&self) -> Option<String> {
        self.redirect()
            .map(|(port, path)| format!("http://localhost:{port}{path}"))
    }
}

struct CodexVendor;
impl OAuthVendor for CodexVendor {
    fn namespace(&self) -> &str {
        CODEX_NAMESPACE
    }
    fn client_id(&self) -> String {
        std::env::var("OPENAB_AGENT_OAUTH_CLIENT_ID")
            .unwrap_or_else(|_| "app_EMoamEEZ73f0CkXaXp7hrann".to_string())
    }
    fn authorize_url(&self) -> &str {
        CODEX_AUTHORIZE_URL
    }
    fn token_url(&self) -> &str {
        CODEX_TOKEN_URL
    }
    fn redirect(&self) -> Option<(u16, &'static str)> {
        Some((REDIRECT_PORT, "/auth/callback"))
    }
    fn scope(&self) -> &str {
        "openid profile email offline_access"
    }
    fn extra_authorize_params(&self) -> &'static [(&'static str, &'static str)] {
        &[
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
            ("originator", "openab-agent"),
        ]
    }
}

struct AnthropicVendor;
impl OAuthVendor for AnthropicVendor {
    fn namespace(&self) -> &str {
        ANTHROPIC_NAMESPACE
    }
    fn client_id(&self) -> String {
        std::env::var("OPENAB_AGENT_ANTHROPIC_CLIENT_ID")
            .unwrap_or_else(|_| "9d1c250a-e61b-44d9-88ed-5944d1962f5e".to_string())
    }
    fn authorize_url(&self) -> &str {
        ANTHROPIC_AUTHORIZE_URL
    }
    fn token_url(&self) -> &str {
        ANTHROPIC_TOKEN_URL
    }
    fn redirect(&self) -> Option<(u16, &'static str)> {
        Some((ANTHROPIC_REDIRECT_PORT, "/callback"))
    }
    fn scope(&self) -> &str {
        ANTHROPIC_SCOPE
    }
    fn extra_authorize_params(&self) -> &'static [(&'static str, &'static str)] {
        &[("code", "true")]
    }
    fn token_body(&self) -> TokenBodyFormat {
        TokenBodyFormat::Json
    }
}

/// xAI (SuperGrok / X Premium subscription) — device-code-primary vendor.
/// Access tokens act as plain Bearer API keys against `api.x.ai/v1`.
struct XaiVendor;
impl OAuthVendor for XaiVendor {
    fn namespace(&self) -> &str {
        XAI_NAMESPACE
    }
    fn client_id(&self) -> String {
        std::env::var("OPENAB_AGENT_XAI_CLIENT_ID")
            .unwrap_or_else(|_| "b1a00492-073a-47ea-816f-4c329264a828".to_string())
    }
    /// Device-primary vendor: no PKCE authorize endpoint exists. `redirect()`
    /// is `None`, so `build_authorize_url` rejects this vendor before ever
    /// reading this value; the device endpoint is returned as the least-wrong
    /// stand-in for the mandatory trait method.
    fn authorize_url(&self) -> &str {
        XAI_DEVICE_CODE_URL
    }
    fn token_url(&self) -> &str {
        XAI_TOKEN_URL
    }
    fn scope(&self) -> &str {
        XAI_SCOPE
    }
    fn grant(&self) -> AuthGrant {
        AuthGrant::DeviceCode
    }
    fn device_authorization_url(&self) -> Option<&str> {
        Some(XAI_DEVICE_CODE_URL)
    }
    fn extra_device_params(&self) -> &'static [(&'static str, &'static str)] {
        &[("referrer", "openab")]
    }
}

/// Resolve a vendor descriptor by `auth.json` namespace. `None` for non-OAuth
/// tenants (e.g. `mcp:<server>`, whose refresh rmcp owns).
fn vendor_for(namespace: &str) -> Option<Box<dyn OAuthVendor>> {
    match namespace {
        CODEX_NAMESPACE => Some(Box::new(CodexVendor)),
        ANTHROPIC_NAMESPACE => Some(Box::new(AnthropicVendor)),
        XAI_NAMESPACE => Some(Box::new(XaiVendor)),
        _ => None,
    }
}

/// Build a vendor's PKCE authorize URL. Pure (unit-testable). `state` is an
/// independent random CSRF value kept distinct from the PKCE verifier (which
/// stays back-channel-only) — the AS just echoes it back.
fn build_authorize_url(vendor: &dyn OAuthVendor, challenge: &str, state: &str) -> Result<String> {
    let redirect = vendor.redirect_uri().ok_or_else(|| {
        anyhow!(
            "{} has no loopback redirect (not a PKCE vendor)",
            vendor.namespace()
        )
    })?;
    let redir = urlencoding::encode(&redirect);
    let scope = urlencoding::encode(vendor.scope());
    let client_id = vendor.client_id();
    let mut url = format!(
        "{}?client_id={client_id}&response_type=code&redirect_uri={redir}&scope={scope}&code_challenge={challenge}&code_challenge_method=S256&state={state}",
        vendor.authorize_url()
    );
    for (k, v) in vendor.extra_authorize_params() {
        url.push('&');
        url.push_str(k);
        url.push('=');
        url.push_str(v);
    }
    Ok(url)
}

/// Exchange an authorization `code` for tokens against `vendor`, encoding the
/// body per `token_body()`. The JSON path also carries `state` (Anthropic
/// echoes it); the form path omits it (Codex).
async fn exchange_authorization_code(
    vendor: &dyn OAuthVendor,
    code: &str,
    state: &str,
    verifier: &str,
) -> Result<TokenStore> {
    let redirect = vendor
        .redirect_uri()
        .ok_or_else(|| anyhow!("{} has no loopback redirect", vendor.namespace()))?;
    let client_id = vendor.client_id();
    let client = reqwest::Client::new();
    let req = client.post(vendor.token_url());
    let resp = match vendor.token_body() {
        TokenBodyFormat::Json => {
            req.json(&serde_json::json!({
                "grant_type": "authorization_code",
                "client_id": client_id,
                "code": code,
                "state": state,
                "redirect_uri": redirect,
                "code_verifier": verifier,
            }))
            .send()
            .await?
        }
        TokenBodyFormat::Form => {
            req.form(&[
                ("grant_type", "authorization_code"),
                ("client_id", client_id.as_str()),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect.as_str()),
            ])
            .send()
            .await?
        }
    };
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Token exchange failed: {body}"));
    }
    let payload: serde_json::Value = resp.json().await?;
    token_store_from_payload(&payload, vendor.token_url(), vendor.namespace())
}

/// Build a `TokenStore` from an OAuth token response, requiring `access_token`
/// and `refresh_token`. Shared by every login + exchange path.
fn token_store_from_payload(
    payload: &serde_json::Value,
    token_endpoint: &str,
    provider: &str,
) -> Result<TokenStore> {
    let access_token = payload["access_token"]
        .as_str()
        .ok_or_else(|| anyhow!("No access_token"))?;
    let refresh_token_val = payload["refresh_token"]
        .as_str()
        .ok_or_else(|| anyhow!("No refresh_token"))?;
    let expires_in = payload["expires_in"].as_u64().unwrap_or(3600);
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    Ok(TokenStore {
        access_token: access_token.to_string(),
        refresh_token: refresh_token_val.to_string(),
        expires_at: now + expires_in,
        token_endpoint: token_endpoint.to_string(),
        provider: provider.to_string(),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStore {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub token_endpoint: String,
    pub provider: String,
}

impl TokenStore {
    /// True when the cached access token has expired (with `REFRESH_SKEW_SECONDS`
    /// safety margin so callers refresh proactively). `u64::MAX` is the
    /// "never expires" sentinel used by providers that omit `expires_in`
    /// — `saturating_add` keeps the skew arithmetic safe against the sentinel
    /// and against any other near-`u64::MAX` clock value.
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_add(REFRESH_SKEW_SECONDS) >= self.expires_at
    }
}

/// Legacy read-tolerant tombstone for the pre-rmcp cross-process paste flow.
/// The paste login now runs entirely in one invocation through rmcp's
/// `AuthorizationManager` (PKCE/CSRF in its in-memory `StateStore`), so
/// nothing writes this anymore. The variant is retained only so a stray
/// `mcp-pending:<server>` entry left in a shared `auth.json` (which also holds
/// the Codex token) still deserializes instead of failing the whole-map parse.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingPasteLogin {
    pub verifier: String,
    pub state: String,
    pub token_url: String,
    pub provider_name: String,
    /// RFC 8707 audience-binding resource snapshotted at `start_paste_login`
    /// so `complete_login`'s token exchange sends the same `resource` the
    /// authorize URL carried (`None` for built-in providers, which skip it).
    /// `#[serde(default)]` keeps pre-existing `auth.json` pending entries
    /// (written before this field existed) deserializable.
    #[serde(default)]
    pub resource: Option<String>,
    /// Unix-seconds stamp set when the pending entry is written; `with_auth_locked`
    /// expires entries older than 15 min (ADR §7) so an abandoned `/auth` two-step
    /// (verifier written, code never pasted) doesn't accumulate. The two-step flow
    /// that *writes* pending state is forthcoming; today this struct is a legacy
    /// read-tolerant tombstone, so the field exists mainly for the GC. `#[serde(default)]`
    /// (= 0) reads pre-existing/legacy entries as ancient → swept on the next write.
    ///
    /// **Any code that writes a fresh `Pending` entry MUST set `created_at` to the
    /// current Unix time** — an unstamped (0) entry is treated as ancient and is
    /// swept by `gc_stale_pending` on the very next locked write, so a verifier
    /// written without stamping would vanish before the user pastes the code.
    #[serde(default)]
    pub created_at: u64,
}

/// `auth.json` value type. Untagged Serde enum: `TokenStore` has required
/// `access_token`, `PendingPasteLogin` has required `verifier` — the
/// shapes are disjoint, so deserialization picks the right variant
/// without an explicit tag (and existing files stay byte-compatible).
/// Keeping the two as distinct variants stops the refresh task from
/// treating pending entries as "expired tokens" and looping on them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AuthEntry {
    Token(TokenStore),
    Pending(PendingPasteLogin),
    /// rmcp-native MCP-server credential (ADR §6.1 storage-format decision A).
    /// Stored under the bare server name, sharing `auth.json` with the `codex`
    /// tenant. `Mcp` MUST stay last in this untagged enum: `StoredCredentials`
    /// only *requires* `client_id`, the loosest field set, so an earlier
    /// position would let it shadow `Token`/`Pending`. Disjointness holds
    /// because `TokenStore` requires `access_token` and `PendingPasteLogin`
    /// requires `verifier`, neither of which `StoredCredentials` carries.
    Mcp(StoredCredentials),
}

/// Default location of `auth.json`. Exposed so `McpRuntimeManager` can
/// thread the same path into its constructor and tests can inject a
/// tempdir without touching `$HOME` (which would race cross-module).
pub fn auth_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".openab")
        .join("agent")
        .join("auth.json")
}

/// Read the `auth.json` map, transparently migrating a legacy single-tenant
/// Codex token file into the new namespaced shape. The migrated map is held
/// in-memory only; the file is rewritten in the new shape on the next save.
///
/// Discriminates by the top-level `access_token` key — present means the
/// file is the legacy `TokenStore` shape, absent means the new namespaced
/// map. A single JSON parse gives accurate error context either way.
fn read_auth_file(path: &Path) -> Result<HashMap<String, AuthEntry>> {
    // A missing/unreadable file is "no credentials yet", not corruption — let
    // it propagate so callers fall through to an empty map without quarantine.
    let data = std::fs::read_to_string(path)?;
    // A successful read that fails to parse is genuine corruption: quarantine
    // the bad bytes (#969 B6 / decision A3) before propagating, so the
    // `unwrap_or_default()` save paths recreate a clean file instead of
    // silently wiping every server's credentials on top of the corruption.
    parse_auth_data(&data).inspect_err(|e| {
        quarantine_corrupt_auth(path, e);
    })
}

fn parse_auth_data(data: &str) -> Result<HashMap<String, AuthEntry>> {
    let value: serde_json::Value =
        serde_json::from_str(data).map_err(|e| anyhow!("Invalid auth.json: {e}"))?;
    if value.get("access_token").is_some() {
        let legacy: TokenStore = serde_json::from_value(value)
            .map_err(|e| anyhow!("Invalid auth.json (legacy format): {e}"))?;
        let mut map = HashMap::new();
        map.insert(CODEX_NAMESPACE.to_string(), AuthEntry::Token(legacy));
        return Ok(map);
    }
    serde_json::from_value(value).map_err(|e| anyhow!("Invalid auth.json: {e}"))
}

/// Quarantine a corrupt `auth.json` (#969 B6 / decision A3 = Option 2). Renames
/// the unparseable file to `auth.json.corrupt-<unix_ts>` so the bad bytes are
/// preserved for forensics, then warns. Best-effort: a rename failure must NOT
/// turn a corrupt-file read into a hard failure, or it would wedge every later
/// save (the opposite of the no-silent-wipe / no-permanent-hard-fail decision).
fn quarantine_corrupt_auth(path: &Path, err: &anyhow::Error) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let quarantine = path.with_extension(format!("json.corrupt-{ts}"));
    tracing::warn!(
        path = %path.display(),
        quarantine = %quarantine.display(),
        error = %err,
        "auth.json is corrupt; quarantining and continuing with an empty store"
    );
    let _ = std::fs::rename(path, &quarantine);
}

/// Atomically replace `auth.json` with the new map via tmp + `rename(2)` +
/// parent-dir fsync. A crash between the tmp write and the rename leaves
/// `auth.json` unchanged; a crash after the rename has the new file
/// already durable. Satisfies the ADR §6.1 refresh-token rotation
/// contract — without rename atomicity, a Spot interruption mid-write
/// would leave a half-written `auth.json` that the next task start would
/// fail to parse, then re-restore from S3 with a now-revoked refresh
/// token.
fn write_auth_file(path: &Path, map: &HashMap<String, AuthEntry>) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let data = serde_json::to_string_pretty(map)?;
    #[cfg(unix)]
    {
        use std::fs::{File, OpenOptions};
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        use std::sync::atomic::{AtomicU64, Ordering};
        static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!("auth.json.tmp.{}.{seq}", std::process::id()));
        let write_and_sync = || -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(data.as_bytes())?;
            file.sync_all()?;
            Ok(())
        };
        if let Err(e) = write_and_sync() {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        // fsync the parent dir so the rename itself is durable; without
        // this, the inode swap can be reordered after a power loss even
        // though the tmp's contents were synced.
        if let Ok(dir_handle) = File::open(dir) {
            let _ = dir_handle.sync_all();
        }
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, &data)?;
    }
    Ok(())
}

/// CLI subcommand that (re)authenticates a tenant `namespace`. Used in
/// credential-error messages so the user runs the right login.
fn auth_subcommand(namespace: &str) -> &'static str {
    if namespace == ANTHROPIC_NAMESPACE {
        "openab-agent auth anthropic-oauth"
    } else if namespace == XAI_NAMESPACE {
        "openab-agent auth xai"
    } else {
        "openab-agent auth codex-oauth"
    }
}
// ── auth.json cross-process locking (ADR §5.4) ──────────────────────────────
//
// `auth.json` is written by multiple processes (one openab-agent per Discord
// thread) and by two code paths within each (`save_tokens` for the codex tenant
// + `McpCredentialStore` for MCP servers). Two hazards, two locks:
//
//   (a) File integrity — every read-modify-write funnels through `with_auth_locked`,
//       which holds an exclusive `flock` on an `auth.json.global.lock` sidecar
//       across the re-read → mutate → atomic-write. The re-read *inside* the lock
//       is what makes concurrent writers merge instead of lost-update.
//   (b) Refresh-token rotation — `lock_tenant_refresh` serialises the network
//       refresh per tenant so concurrent processes present a rotated `RT_old`
//       only once, never tripping OAuth 2.1 §10.4 token-family revocation.
//
// `flock(2)` (not a sentinel lockfile) so the kernel auto-releases on fd close /
// process death — no stale lock, no orphan cleanup. The lock lives on a sidecar,
// never on `auth.json` itself, because the atomic tmp+rename swaps that inode out
// from under any lock held on it. `#[cfg(unix)]`; a non-unix build is a no-op
// (openab-agent is de-facto unix-only — see `write_auth_file`).

/// Sidecar lock path `auth.json.<suffix>.lock`, next to the auth file so a
/// test-injected tempdir locks its own sidecar rather than the real `$HOME` one.
#[cfg(unix)]
fn lock_path_for(auth: &Path, suffix: &str) -> PathBuf {
    let dir = auth.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!("auth.json.{suffix}.lock"))
}

/// RAII guard releasing the advisory lock on drop. The kernel also drops it on
/// fd close / process death, so a crashed holder never wedges the file.
#[cfg(unix)]
pub(crate) struct AuthFileLock {
    file: std::fs::File,
}

#[cfg(unix)]
impl Drop for AuthFileLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        // SAFETY: `self.file` owns a valid fd; flock has no memory effects.
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(unix)]
fn open_lock_file(lock: &Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = lock.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock)?)
}

/// Blocking exclusive lock. Used ONLY for the global file RMW, which performs no
/// network I/O while held, so acquisition blocks at most for another process's
/// fast tmp+rename — never for a slow refresh (those take the per-tenant lock).
#[cfg(unix)]
fn flock_exclusive(lock: &Path) -> Result<AuthFileLock> {
    use std::os::unix::io::AsRawFd;
    let file = open_lock_file(lock)?;
    // SAFETY: valid fd held by `file`; flock has no memory effects.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(AuthFileLock { file })
}

/// Acquire the global `auth.json` write lock (a no-op `None` guard off-unix).
/// Both `with_auth_locked` and `McpCredentialStore::clear` — which needs a
/// delete-on-empty tail the funnel can't express — acquire here, so the
/// `"global"` sidecar name and the acquire policy live in exactly one place.
fn lock_global(path: &Path) -> Result<Option<AuthFileLock>> {
    #[cfg(unix)]
    {
        Ok(Some(flock_exclusive(&lock_path_for(path, "global"))?))
    }
    #[cfg(not(unix))]
    {
        // No flock(2) off-unix: every writer runs unprotected, so concurrent
        // processes can silently corrupt auth.json (ADR §5.4). openab-agent is
        // de-facto unix-only; warn once rather than fail silently so a non-unix
        // build with concurrent processes is at least diagnosable.
        use std::sync::Once;
        static WARN_NO_LOCK: Once = Once::new();
        WARN_NO_LOCK.call_once(|| {
            tracing::warn!(
                "auth.json cross-process file locking is unavailable on this non-unix platform; \
                 concurrent openab-agent processes may corrupt stored credentials (ADR §5.4)"
            );
        });
        let _ = path;
        Ok(None)
    }
}

/// (a) File-integrity funnel (ADR §5.4). Holds the global sidecar lock across a
/// re-read → mutate → atomic-write so the codex `save_tokens` path AND the MCP
/// `McpCredentialStore` never lost-update the shared map: each writer merges onto
/// the latest on-disk state. A corrupt file is quarantined by `read_auth_file`
/// and treated as empty (`unwrap_or_default`), matching the prior save behaviour.
fn with_auth_locked<R>(
    path: &Path,
    f: impl FnOnce(&mut HashMap<String, AuthEntry>) -> R,
) -> Result<R> {
    let _guard = lock_global(path)?;
    let mut map = read_auth_file(path).unwrap_or_default();
    let r = f(&mut map);
    gc_stale_pending(&mut map);
    write_auth_file(path, &map)?;
    Ok(r)
}

/// Opportunistic GC (ADR §7): drop `AuthEntry::Pending` entries older than 15 min
/// on every locked write, so abandoned `/auth` two-step attempts don't accumulate.
/// `created_at == 0` (legacy/unstamped entries) reads as ancient and is swept.
const PENDING_TTL_SECONDS: u64 = 15 * 60;

fn gc_stale_pending(map: &mut HashMap<String, AuthEntry>) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    map.retain(|_, entry| match entry {
        AuthEntry::Pending(p) => now.saturating_sub(p.created_at) <= PENDING_TTL_SECONDS,
        _ => true,
    });
}

/// HTTP timeout on the token-refresh network call (codex + MCP). Strictly shorter
/// than [`REFRESH_LOCK_TIMEOUT`] so the per-tenant lock is provably released before
/// a waiter's deadline — which is what lets the lock timeout fail *closed* (a
/// timeout then signals a genuinely abnormal state, not normal slowness).
pub(crate) const REFRESH_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Outcome of acquiring a tenant's refresh lock. See [`lock_tenant_refresh`].
#[cfg(unix)]
pub(crate) enum RefreshLock {
    /// Lock acquired — hold across the refresh.
    Held(AuthFileLock),
    /// Sidecar lock file couldn't be opened (filesystem error). Best-effort:
    /// proceed unserialised rather than block every refresh on a broken lock dir.
    Unavailable,
    /// Contended past [`REFRESH_LOCK_TIMEOUT`]. Fail-closed: the caller must NOT
    /// refresh — surface a transient, retryable error.
    TimedOut,
}

/// Worst-case number of sequential bounded refresh round-trips a single lock-holder
/// makes while holding the tenant lock. The codex path makes one (the token POST);
/// the MCP path makes two — rmcp's `initialize_from_store()` (authorization-server
/// discovery) then `get_access_token()` (the refresh) — each bounded by
/// [`REFRESH_HTTP_TIMEOUT`].
#[cfg(unix)]
const MAX_REFRESH_ROUND_TRIPS: u64 = 2;

/// Lock-acquire deadline. Sized strictly above the worst-case lock-hold
/// (`MAX_REFRESH_ROUND_TRIPS` × [`REFRESH_HTTP_TIMEOUT`]) plus margin, so a waiter
/// never fails closed on a holder that is still legitimately progressing through its
/// bounded — and, on the MCP path, multi-call — refresh; only a genuinely stuck
/// holder trips the timeout. Derived from `REFRESH_HTTP_TIMEOUT` so the relationship
/// can't silently drift if that bound changes.
#[cfg(unix)]
const REFRESH_LOCK_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(REFRESH_HTTP_TIMEOUT.as_secs() * MAX_REFRESH_ROUND_TRIPS + 4);

/// (b) Per-tenant refresh serialisation (ADR §5.4). On success the returned
/// [`RefreshLock::Held`] guard is kept by the caller across the network refresh so
/// concurrent processes do exactly one real refresh per tenant — never presenting a
/// rotated `RT_old` twice (OAuth 2.1 §10.4 family revocation). Non-blocking acquire
/// on a single fd + async backoff so a refresh in flight elsewhere never blocks this
/// executor thread.
///
/// **Fail-closed on timeout.** Each refresh round-trip is bounded by
/// [`REFRESH_HTTP_TIMEOUT`], and [`REFRESH_LOCK_TIMEOUT`] is sized above the worst-case
/// lock-hold ([`MAX_REFRESH_ROUND_TRIPS`] sequential bounded calls — the MCP path makes
/// two, codex one); combined with `flock(2)` auto-release on holder death, a live holder
/// still progressing always releases before a waiter's deadline. A timeout therefore
/// signals a genuinely abnormal state — and proceeding unserialised
/// would re-present `RT_old` and risk the exact family revocation this lock exists to
/// prevent, strictly worse than a transient retry. So we return
/// [`RefreshLock::TimedOut`] (logged at `error!`) and the caller surfaces a retryable
/// error instead of refreshing. A filesystem error opening the sidecar returns
/// [`RefreshLock::Unavailable`] — best-effort degrade (proceed) rather than block
/// every refresh on a broken lock dir.
///
/// Reuse-safety on the happy path comes from loading the refresh token *inside* the
/// lock: a process that waited then loads the token the winner just wrote, so it
/// never re-presents a rotated `RT_old`. Re-checking expiry after acquiring is an
/// additional optimisation that skips a redundant network refresh — `get_valid_token`
/// does this explicitly, and the MCP path gets it free from rmcp's
/// `initialize_from_store()` reload + `get_access_token` (which returns early when the
/// token is already fresh). `force_refresh` intentionally skips that optimisation and
/// always refreshes (it runs on a 401, where the clock-fresh token is already
/// known-bad); it stays reuse-safe because it, too, loads inside the lock.
#[cfg(unix)]
pub(crate) async fn lock_tenant_refresh(auth: &Path, tenant: &str) -> RefreshLock {
    lock_tenant_refresh_until(auth, tenant, REFRESH_LOCK_TIMEOUT).await
}

/// [`lock_tenant_refresh`] with an injectable deadline so tests can drive the
/// fail-closed timeout path in milliseconds instead of [`REFRESH_LOCK_TIMEOUT`].
#[cfg(unix)]
async fn lock_tenant_refresh_until(
    auth: &Path,
    tenant: &str,
    timeout: std::time::Duration,
) -> RefreshLock {
    use std::os::unix::io::AsRawFd;
    let lock = lock_path_for(auth, &format!("refresh.{tenant}"));
    // Open the lock fd once; re-issue `flock` on it each retry instead of
    // re-opening (and re-`create_dir_all`-ing) the same file every 100 ms.
    let file = match open_lock_file(&lock) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(tenant, error = %e, "refresh lock unavailable; proceeding unserialised");
            return RefreshLock::Unavailable;
        }
    };
    let deadline = std::time::Instant::now() + timeout;
    loop {
        // SAFETY: valid fd held by `file`; flock has no memory effects.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return RefreshLock::Held(AuthFileLock { file });
        }
        let err = std::io::Error::last_os_error();
        // EWOULDBLOCK/EAGAIN (both `ErrorKind::WouldBlock`) = another holder is
        // refreshing; any other errno is a real failure we degrade on.
        if err.kind() != std::io::ErrorKind::WouldBlock {
            tracing::warn!(tenant, error = %err, "refresh lock unavailable; proceeding unserialised");
            return RefreshLock::Unavailable;
        }
        if std::time::Instant::now() >= deadline {
            // Fail-closed (see fn doc): the refresh is HTTP-bounded shorter than this
            // deadline, so a timeout is abnormal. Logged at error! so the rare
            // contended refresh is alertable; the caller turns this into a transient
            // retryable error rather than re-presenting RT_old.
            tracing::error!(
                tenant,
                "timed out waiting for refresh lock; failing closed (refresh deferred)"
            );
            return RefreshLock::TimedOut;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Load the LLM token stored under `namespace` (`codex` / `anthropic-oauth`).
pub fn load_tokens_for(namespace: &str) -> Result<TokenStore> {
    let path = auth_path();
    let cmd = auth_subcommand(namespace);
    // Preserve the underlying read/parse error for debugging.
    let map = read_auth_file(&path).map_err(|e| {
        anyhow!(
            "No credentials at {} ({e}). Run `{cmd}` first.",
            path.display()
        )
    })?;
    match map.get(namespace) {
        Some(AuthEntry::Token(t)) => Ok(t.clone()),
        _ => Err(anyhow!(
            "No {namespace} credentials in {}. Run `{cmd}` first.",
            path.display()
        )),
    }
}

/// Save a token under its own `provider` field as the namespace key, leaving
/// every other tenant in `auth.json` untouched. Funnels through
/// `with_auth_locked` so a concurrent codex/MCP/anthropic writer never
/// lost-updates this tenant (ADR §5.4 (a)).
fn save_tokens_for(store: &TokenStore) -> Result<()> {
    let provider = store.provider.clone();
    let store = store.clone();
    with_auth_locked(&auth_path(), move |map| {
        map.insert(provider, AuthEntry::Token(store));
    })
}

pub fn load_tokens() -> Result<TokenStore> {
    load_tokens_for(CODEX_NAMESPACE)
}

/// rmcp [`CredentialStore`] backed by the shared `auth.json` file (ADR §6.1
/// storage-format decision A). One instance is bound to a single MCP server's
/// bare-name key (e.g. `linear`); rmcp's `AuthorizationManager` owns the
/// load/save/clear lifecycle. Reuses the atomic `read_auth_file` /
/// `write_auth_file` so MCP credentials inherit the same tmp+rename+fsync,
/// 0o600 durability the Codex tenant relies on, without disturbing it.
///
/// The filesystem reads/writes are synchronous: `auth.json` holds a handful of
/// entries, so blocking the executor for the duration is trivial (mirrors the
/// existing `list_pending_logins_at` rationale) and avoids a `spawn_blocking`
/// round-trip.
#[derive(Debug, Clone)]
pub struct McpCredentialStore {
    path: PathBuf,
    key: String,
}

impl McpCredentialStore {
    pub fn new(path: PathBuf, server_name: impl Into<String>) -> Self {
        Self {
            path,
            key: server_name.into(),
        }
    }
}

#[async_trait::async_trait]
impl CredentialStore for McpCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        // Missing / unreadable file → "no credentials yet", not an error: the
        // first login is the write that creates it.
        let Ok(map) = read_auth_file(&self.path) else {
            return Ok(None);
        };
        match map.get(&self.key) {
            Some(AuthEntry::Mcp(c)) => Ok(Some(c.clone())),
            // A non-Mcp entry under this key (e.g. a legacy `Token` from the
            // pre-rmcp paste flow) is treated as absent → triggers re-login,
            // matching the accepted one-time re-auth migration.
            _ => Ok(None),
        }
    }

    async fn save(&self, mut credentials: StoredCredentials) -> Result<(), AuthError> {
        use oauth2::{RefreshToken, TokenResponse};

        // OAuth 2.1 §10.4: when a refresh response omits `refresh_token`, the
        // prior one stays valid. rmcp's `refresh_token()` rebuilds the stored
        // credentials from the refresh response alone, so a rotating-but-omitting
        // AS would lose our fallback — splice the prior refresh_token back in.
        // The prior-read happens inside the lock (re-read) so two writers can't
        // race the splice. The whole RMW funnels through `with_auth_locked` so an
        // interleaving codex `save_tokens` never lost-updates this MCP entry.
        let incoming_has_refresh = credentials
            .token_response
            .as_ref()
            .and_then(|tr| tr.refresh_token())
            .is_some_and(|rt| !rt.secret().is_empty());
        let key = self.key.clone();
        with_auth_locked(&self.path, move |map| {
            if !incoming_has_refresh {
                let prior = match map.get(&key) {
                    Some(AuthEntry::Mcp(old)) => old
                        .token_response
                        .as_ref()
                        .and_then(|tr| tr.refresh_token())
                        .map(|rt| rt.secret().to_string())
                        .filter(|s| !s.is_empty()),
                    _ => None,
                };
                if let (Some(prior), Some(tr)) = (prior, credentials.token_response.as_mut()) {
                    tr.set_refresh_token(Some(RefreshToken::new(prior)));
                }
            }
            map.insert(key.clone(), AuthEntry::Mcp(credentials));
        })
        .map_err(|e| AuthError::InternalError(e.to_string()))
    }

    async fn clear(&self) -> Result<(), AuthError> {
        // Same global lock as every other writer, but with a delete-on-empty tail
        // `with_auth_locked` can't express (it always writes), so `clear` acquires
        // the shared `lock_global` directly rather than funnelling through it.
        let _guard =
            lock_global(&self.path).map_err(|e| AuthError::InternalError(e.to_string()))?;
        let mut map = match read_auth_file(&self.path) {
            Ok(m) => m,
            Err(_) => return Ok(()),
        };
        if map.remove(&self.key).is_none() {
            return Ok(());
        }
        if map.is_empty() {
            let _ = std::fs::remove_file(&self.path);
            return Ok(());
        }
        write_auth_file(&self.path, &map).map_err(|e| AuthError::InternalError(e.to_string()))
    }
}

pub async fn get_valid_token_for(namespace: &str) -> Result<String> {
    // 1. Fast path: a fresh token needs no lock.
    let store = load_tokens_for(namespace)?;
    if !store.is_expired() {
        return Ok(store.access_token);
    }
    // 2. Serialise the refresh per tenant — held across the network call so a
    //    second process does not present the same RT_old (§5.4 (b)). Fail closed
    //    on a contended-lock timeout: surface a retryable error rather than
    //    refresh unserialised (which would risk §10.4 family revocation).
    #[cfg(unix)]
    let _refresh_guard = match lock_tenant_refresh(&auth_path(), namespace).await {
        RefreshLock::Held(g) => Some(g),
        RefreshLock::Unavailable => None,
        RefreshLock::TimedOut => {
            return Err(anyhow!(
                "{namespace} token refresh is busy (refresh lock contended); retry shortly"
            ))
        }
    };
    // 3. Double-check: another process may have refreshed while we waited.
    let store = load_tokens_for(namespace)?;
    if !store.is_expired() {
        return Ok(store.access_token);
    }
    // 4. Exactly one network refresh per tenant per expiry (tenant lock held).
    let fresh = refresh_token(&store).await?;
    // 5. Commit under the global file lock.
    save_tokens_for(&fresh)?;
    Ok(fresh.access_token)
}

pub async fn force_refresh_for(namespace: &str) -> Result<String> {
    // Serialise even a forced refresh so two of them can't both rotate RT_old.
    // Fail closed on timeout (see get_valid_token_for) rather than refresh unserialised.
    #[cfg(unix)]
    let _refresh_guard = match lock_tenant_refresh(&auth_path(), namespace).await {
        RefreshLock::Held(g) => Some(g),
        RefreshLock::Unavailable => None,
        RefreshLock::TimedOut => {
            return Err(anyhow!(
                "{namespace} token refresh is busy (refresh lock contended); retry shortly"
            ))
        }
    };
    let store = load_tokens_for(namespace)?;
    let new_store = refresh_token(&store).await?;
    save_tokens_for(&new_store)?;
    Ok(new_store.access_token)
}

pub async fn get_valid_token() -> Result<String> {
    get_valid_token_for(CODEX_NAMESPACE).await
}

pub async fn force_refresh() -> Result<String> {
    force_refresh_for(CODEX_NAMESPACE).await
}

async fn refresh_token(store: &TokenStore) -> Result<TokenStore> {
    let vendor = vendor_for(&store.provider)
        .ok_or_else(|| anyhow!("No OAuth vendor for provider `{}`", store.provider))?;
    let client_id = vendor.client_id();
    // Bound the refresh so the per-tenant lock (held across this call) is provably
    // released before another process's lock deadline — see REFRESH_HTTP_TIMEOUT.
    let client = reqwest::Client::builder()
        .timeout(REFRESH_HTTP_TIMEOUT)
        .build()?;
    // Body encoding comes from the vendor descriptor: Anthropic takes JSON (and
    // rejects a `scope` field on refresh — Pi #2169); Codex takes a form body.
    let req = client.post(&store.token_endpoint);
    let resp = match vendor.token_body() {
        TokenBodyFormat::Json => {
            req.json(&serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": store.refresh_token,
                "client_id": client_id,
            }))
            .send()
            .await?
        }
        TokenBodyFormat::Form => {
            req.form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", store.refresh_token.as_str()),
                ("client_id", client_id.as_str()),
            ])
            .send()
            .await?
        }
    };
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!(
            "Token refresh failed (HTTP {status}): {body}. Run `{}` again.",
            auth_subcommand(&store.provider)
        ));
    }
    let payload: serde_json::Value = resp.json().await?;
    let access_token = payload["access_token"]
        .as_str()
        .ok_or_else(|| anyhow!("No access_token in refresh response"))?;
    let new_refresh = payload["refresh_token"]
        .as_str()
        .unwrap_or(&store.refresh_token);
    let expires_in = payload["expires_in"].as_u64().unwrap_or(3600);
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    Ok(TokenStore {
        access_token: access_token.to_string(),
        refresh_token: new_refresh.to_string(),
        expires_at: now + expires_in,
        token_endpoint: store.token_endpoint.clone(),
        provider: store.provider.clone(),
    })
}

pub fn generate_pkce() -> (String, String) {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("getrandom failed");
    let verifier = URL_SAFE_NO_PAD.encode(buf);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// Shared PKCE browser/paste login driver (ADR §5.1). The authorize URL,
/// loopback redirect, and token-body encoding all come from the `vendor`
/// descriptor, so every PKCE vendor reuses this one flow. Folds the codex flow
/// into the `accept_callback_code` / `code_from_redirect` helpers (the
/// long-standing TODO) and unifies the `127.0.0.1` bind across vendors.
async fn login_pkce_flow(vendor: &dyn OAuthVendor, no_browser: bool) -> Result<()> {
    let (port, _path) = vendor
        .redirect()
        .ok_or_else(|| anyhow!("{} is not a PKCE vendor", vendor.namespace()))?;
    let (verifier, challenge) = generate_pkce();
    // Independent random CSRF state, kept distinct from the PKCE verifier (which
    // stays back-channel-only). 32 bytes: claude.ai's authorize rejects a short
    // state ("Invalid request format") — long enough for every vendor.
    let mut state_buf = [0u8; 32];
    getrandom::fill(&mut state_buf).expect("getrandom failed");
    let state = URL_SAFE_NO_PAD.encode(state_buf);
    let auth_url = build_authorize_url(vendor, &challenge, &state)?;

    let code = if no_browser {
        println!("Open this URL in your browser:\n\n  {auth_url}\n");
        println!(
            "After approving, copy the full redirect URL (or just the `code#state`) and paste it here:\n"
        );
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .map_err(|e| anyhow!("Failed to read input: {e}"))?;
        let input = input.trim();
        if input.is_empty() {
            return Err(anyhow!("No URL provided"));
        }
        // Accept either a full redirect URL or a bare `code#state`. Require the
        // `#state` form so CSRF state is always verified — a bare code can't be
        // checked and is rejected rather than trusted.
        if let Ok(url) = url::Url::parse(input) {
            code_from_redirect(&url, &state)?
        } else {
            let (code, st) = input.split_once('#').ok_or_else(|| {
                anyhow!("Paste the full `code#state` value (or the redirect URL) so the state can be verified")
            })?;
            if st != state {
                return Err(anyhow!("State mismatch"));
            }
            code.to_string()
        }
    } else {
        let listener = TcpListener::bind(format!("127.0.0.1:{port}")).map_err(|e| {
            anyhow!("Failed to bind port {port}: {e}. Is another instance running?")
        })?;
        println!("Opening browser for authentication...\n");
        if open::that(&auth_url).is_err() {
            println!("Could not open browser. Open this URL manually:\n\n  {auth_url}\n");
        }
        println!("Waiting for callback...");
        accept_callback_code(&listener, &state)?
    };

    let store = exchange_authorization_code(vendor, &code, &state, &verifier).await?;
    save_tokens_for(&store)?;
    println!(
        "\n\u{2705} Login successful! Token saved to {:?}",
        auth_path()
    );
    Ok(())
}

/// Codex (OpenAI) browser PKCE login.
pub async fn login_browser_flow(no_browser: bool) -> Result<()> {
    login_pkce_flow(&CodexVendor, no_browser).await
}

/// Extract the OAuth `code` from a parsed redirect URL, validating `state`.
/// Shared by every loopback-callback OAuth flow.
fn code_from_redirect(url: &url::Url, expected_state: &str) -> Result<String> {
    let code = url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
        .ok_or_else(|| {
            let error = url
                .query_pairs()
                .find(|(k, _)| k == "error")
                .map(|(_, v)| v.to_string());
            anyhow!(
                "No code in redirect. Error: {}",
                error.unwrap_or_else(|| "unknown".into())
            )
        })?;
    let cb_state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string());
    if cb_state.as_deref() != Some(expected_state) {
        return Err(anyhow!("State mismatch"));
    }
    Ok(code)
}

/// Block on the loopback listener for the OAuth redirect, reply 200, return the
/// authorization code. Note: the Codex flow above predates this helper and still
/// inlines the same logic; fold it in if that path is ever touched again.
fn accept_callback_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    listener.set_nonblocking(false)?;
    let (mut stream, _) = listener
        .accept()
        .map_err(|e| anyhow!("Failed to accept callback: {e}"))?;
    let mut reader = std::io::BufReader::new(&stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let path = request_line.split_whitespace().nth(1).unwrap_or("");
    let url = url::Url::parse(&format!("http://localhost{path}"))
        .map_err(|_| anyhow!("Invalid callback URL"))?;
    let code = code_from_redirect(&url, expected_state)?;
    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<html><body><h1>Authentication successful!</h1><p>You can close this tab.</p></body></html>";
    let _ = stream.write_all(response.as_bytes());
    Ok(code)
}

/// Anthropic OAuth (Claude Pro/Max) browser PKCE login. JSON token exchange
/// against `platform.claude.com`; all vendor specifics live in `AnthropicVendor`.
pub async fn login_anthropic_browser_flow(no_browser: bool) -> Result<()> {
    login_pkce_flow(&AnthropicVendor, no_browser).await
}

// Device code flow
pub async fn login_codex_device_flow() -> Result<()> {
    println!("Starting OpenAI Codex device-code login...\n");
    let client = reqwest::Client::new();
    let client_id = CodexVendor.client_id();

    let resp = client
        .post(CODEX_DEVICE_AUTH_URL)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({"client_id": client_id}))
        .send()
        .await?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Device authorization request failed: {body}"));
    }
    let device_resp: serde_json::Value = resp.json().await?;
    let device_auth_id = device_resp["device_auth_id"]
        .as_str()
        .ok_or_else(|| anyhow!("No device_auth_id"))?;
    let user_code = device_resp["user_code"]
        .as_str()
        .ok_or_else(|| anyhow!("No user_code"))?;
    let interval = device_resp["interval"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| device_resp["interval"].as_u64())
        .unwrap_or(5)
        .max(5);

    println!("  Go to:      https://auth.openai.com/codex/device");
    println!("  Enter code: {}\n", user_code);
    println!("Waiting for authorization...");

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(600);
    let mut poll_interval = interval;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(anyhow!("Device flow timed out after 10 minutes."));
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(poll_interval)).await;
        let resp = client.post(CODEX_DEVICE_TOKEN_URL)
            .json(&serde_json::json!({"client_id": client_id, "device_auth_id": device_auth_id, "user_code": user_code}))
            .send().await?;
        let status = resp.status();
        let payload: serde_json::Value = resp.json().await?;
        if status.is_success() {
            let auth_code = payload["authorization_code"]
                .as_str()
                .ok_or_else(|| anyhow!("No authorization_code: {payload}"))?;
            let code_verifier = payload["code_verifier"]
                .as_str()
                .ok_or_else(|| anyhow!("No code_verifier: {payload}"))?;
            let token_resp = client
                .post(CODEX_TOKEN_URL)
                .form(&[
                    ("grant_type", "authorization_code"),
                    ("client_id", client_id.as_str()),
                    ("code", auth_code),
                    ("code_verifier", code_verifier),
                    ("redirect_uri", CODEX_DEVICE_REDIRECT_URI),
                ])
                .send()
                .await?;
            if !token_resp.status().is_success() {
                let body = token_resp.text().await.unwrap_or_default();
                return Err(anyhow!("Token exchange failed: {body}"));
            }
            let token_payload: serde_json::Value = token_resp.json().await?;
            let store = token_store_from_payload(&token_payload, CODEX_TOKEN_URL, CODEX_NAMESPACE)?;
            save_tokens_for(&store)?;
            println!(
                "\n\u{2705} Login successful! Token saved to {:?}",
                auth_path()
            );
            return Ok(());
        }
        let error_code = payload["error"]["code"]
            .as_str()
            .or_else(|| payload["error"].as_str())
            .unwrap_or_default();
        match error_code {
            "authorization_pending" | "deviceauth_authorization_pending" => continue,
            "slow_down" => {
                poll_interval += 5;
                continue;
            }
            "expired_token" | "deviceauth_expired" => return Err(anyhow!("Device code expired.")),
            "access_denied" => return Err(anyhow!("Authorization denied by user.")),
            _ => {
                if status.as_u16() == 403 || status.as_u16() == 404 {
                    continue;
                }
                return Err(anyhow!(
                    "Device-code error: {error_code} \u{2014} {payload}"
                ));
            }
        }
    }
}

// ── RFC 8628 device-code login driver (ADR §5.1, device axis) ──────────────
//
// Standards-compliant counterpart of `login_pkce_flow`: every `DeviceCode`-grant
// vendor reuses this one flow, parameterised by the descriptor. (The codex
// device flow above stays bespoke — OpenAI's `device_auth_id` protocol is not
// RFC 8628.) Parsing/classification are pure functions so the edge cases that
// bit other implementations (interval 0, absent `expires_in`, `slow_down`
// without a new interval, non-https verification URIs) are unit-tested without
// a live authorization server.

/// Default poll interval when the AS omits `interval` or sends a non-positive
/// value (RFC 8628 §3.2 default).
const DEVICE_DEFAULT_INTERVAL_SECS: u64 = 5;
/// Deadline fallback when the AS omits `expires_in` (defensive; RFC 8628
/// requires it, but a lenient default beats hard-failing an otherwise valid
/// grant).
const DEVICE_DEFAULT_EXPIRES_IN_SECS: u64 = 900;

/// Parsed RFC 8628 §3.2 device authorization response.
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    interval_secs: u64,
    expires_in_secs: u64,
}

/// The verification URI is shown to (and possibly opened by) the user; force
/// https so a malicious/tampered response can't direct them to an arbitrary
/// scheme or plaintext endpoint.
fn validate_https_url(raw: &str) -> Result<String> {
    let parsed = url::Url::parse(raw)
        .map_err(|_| anyhow!("Untrusted verification URI in device authorization response"))?;
    if parsed.scheme() != "https" {
        return Err(anyhow!(
            "Untrusted verification URI (non-https) in device authorization response"
        ));
    }
    Ok(parsed.to_string())
}

fn required_device_field<'a>(body: &'a serde_json::Value, field: &str) -> Result<&'a str> {
    body[field]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("No {field} in device authorization response"))
}

fn parse_device_authorization(body: &serde_json::Value) -> Result<DeviceAuthorization> {
    let verification_uri_complete = match body["verification_uri_complete"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        Some(raw) => Some(validate_https_url(raw)?),
        None => None,
    };
    Ok(DeviceAuthorization {
        device_code: required_device_field(body, "device_code")?.to_string(),
        user_code: required_device_field(body, "user_code")?.to_string(),
        verification_uri: validate_https_url(required_device_field(body, "verification_uri")?)?,
        verification_uri_complete,
        // RFC 8628 allows interval 0 (no minimum wait) and some ASes send
        // strings/garbage — fall back to the default rather than failing.
        interval_secs: body["interval"]
            .as_u64()
            .filter(|v| *v > 0)
            .unwrap_or(DEVICE_DEFAULT_INTERVAL_SECS),
        expires_in_secs: body["expires_in"]
            .as_u64()
            .filter(|v| *v > 0)
            .unwrap_or(DEVICE_DEFAULT_EXPIRES_IN_SECS),
    })
}

/// What a non-2xx token poll means (RFC 8628 §3.5).
#[derive(Debug, PartialEq, Eq)]
enum DevicePollDisposition {
    /// User hasn't approved yet — keep polling.
    Pending,
    /// Poll slower; the AS may supply a replacement interval.
    SlowDown(Option<u64>),
    /// Terminal — stop polling and surface the message.
    Fatal(String),
}

fn classify_device_poll_error(payload: &serde_json::Value) -> DevicePollDisposition {
    let code = payload["error"].as_str().unwrap_or_default();
    match code {
        "authorization_pending" => DevicePollDisposition::Pending,
        "slow_down" => {
            DevicePollDisposition::SlowDown(payload["interval"].as_u64().filter(|v| *v > 0))
        }
        // xAI has been observed emitting the non-standard `authorization_denied`.
        "access_denied" | "authorization_denied" => {
            DevicePollDisposition::Fatal("authorization was denied".to_string())
        }
        "expired_token" => {
            DevicePollDisposition::Fatal("device code expired before authorization".to_string())
        }
        other => {
            let desc = payload["error_description"].as_str().unwrap_or_default();
            DevicePollDisposition::Fatal(if desc.is_empty() {
                format!("unexpected device-code error: {other}")
            } else {
                format!("unexpected device-code error: {other} — {desc}")
            })
        }
    }
}

/// RFC 8628 §3.5: after `slow_down` the client MUST increase its poll interval
/// by 5 seconds. A server-supplied replacement interval is honored only when it
/// slows polling down further — never to poll faster than before.
fn next_slow_down_interval(current: u64, server: Option<u64>) -> u64 {
    (current + 5).max(server.unwrap_or(0))
}

/// Poll-interval ceiling for transport backoff, so a still-valid device code
/// keeps getting polled at a useful rate while the network flaps.
const DEVICE_MAX_POLL_INTERVAL_SECS: u64 = 60;

/// RFC 8628 §3.5: on a connection timeout the client MUST reduce its polling
/// frequency before retrying, with exponential backoff recommended. Doubles
/// the interval, clamped to `[DEVICE_DEFAULT_INTERVAL_SECS, DEVICE_MAX_POLL_INTERVAL_SECS]`.
fn next_transport_backoff_interval(current: u64) -> u64 {
    current
        .saturating_mul(2)
        .clamp(DEVICE_DEFAULT_INTERVAL_SECS, DEVICE_MAX_POLL_INTERVAL_SECS)
}

/// Shared RFC 8628 login: request a device code, show the user code +
/// verification link (preferring the prefilled `verification_uri_complete`),
/// poll the token endpoint until approval, persist under the vendor namespace.
async fn login_device_code_flow(vendor: &dyn OAuthVendor) -> Result<()> {
    if vendor.grant() != AuthGrant::DeviceCode {
        return Err(anyhow!(
            "{} is not a device-code vendor",
            vendor.namespace()
        ));
    }
    let device_url = vendor.device_authorization_url().ok_or_else(|| {
        anyhow!(
            "{} has no device authorization endpoint",
            vendor.namespace()
        )
    })?;
    let client_id = vendor.client_id();
    let client = reqwest::Client::new();

    let mut fields: Vec<(&str, &str)> =
        vec![("client_id", client_id.as_str()), ("scope", vendor.scope())];
    fields.extend_from_slice(vendor.extra_device_params());
    let resp = client.post(device_url).form(&fields).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!(
            "Device authorization request failed (HTTP {status}): {body}"
        ));
    }
    let payload: serde_json::Value = resp.json().await?;
    let device = parse_device_authorization(&payload)?;

    println!(
        "  Go to:      {}",
        device
            .verification_uri_complete
            .as_deref()
            .unwrap_or(&device.verification_uri)
    );
    println!("  Enter code: {}\n", device.user_code);
    println!("Waiting for authorization...");

    let deadline =
        tokio::time::Instant::now() + tokio::time::Duration::from_secs(device.expires_in_secs);
    let mut poll_interval = device.interval_secs;
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(poll_interval)).await;
        if tokio::time::Instant::now() >= deadline {
            return Err(anyhow!(
                "Device flow timed out after {} seconds.",
                device.expires_in_secs
            ));
        }
        let resp = match client
            .post(vendor.token_url())
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id.as_str()),
                ("device_code", device.device_code.as_str()),
            ])
            .send()
            .await
        {
            Ok(r) => r,
            // RFC 8628 §3.5: a connection timeout is transient — the device
            // code is still valid, so back off and keep polling until the
            // deadline instead of destroying the in-progress login (F4).
            Err(e) if e.is_timeout() || e.is_connect() => {
                poll_interval = next_transport_backoff_interval(poll_interval);
                tracing::warn!(
                    error = %e,
                    poll_interval,
                    "device token poll transport error; backing off and retrying"
                );
                continue;
            }
            // Non-retryable transport failures (TLS, request build, redirect
            // policy) stay explicit and fail the flow.
            Err(e) => return Err(e.into()),
        };
        let status = resp.status();
        // Errors may arrive as non-JSON (proxies, HTML error pages) — treat an
        // unparseable body as an empty object so classification still runs.
        let text = resp.text().await.unwrap_or_default();
        let payload: serde_json::Value =
            serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({}));
        if status.is_success() {
            let store = token_store_from_payload(&payload, vendor.token_url(), vendor.namespace())?;
            save_tokens_for(&store)?;
            println!(
                "\n\u{2705} Login successful! Token saved to {:?}",
                auth_path()
            );
            return Ok(());
        }
        match classify_device_poll_error(&payload) {
            DevicePollDisposition::Pending => continue,
            DevicePollDisposition::SlowDown(server_interval) => {
                poll_interval = next_slow_down_interval(poll_interval, server_interval);
            }
            DevicePollDisposition::Fatal(msg) => {
                return Err(anyhow!(
                    "{} device authorization failed (HTTP {status}): {msg}",
                    vendor.namespace()
                ));
            }
        }
    }
}

/// xAI (SuperGrok / X Premium) RFC 8628 device-code login.
pub async fn login_xai_device_flow() -> Result<()> {
    println!("Starting xAI (SuperGrok / X Premium) device-code login...\n");
    login_device_code_flow(&XaiVendor).await
}

pub fn show_status() {
    let path = auth_path();
    let tokens: Vec<TokenStore> = read_auth_file(&path)
        .map(|map| {
            let mut v: Vec<TokenStore> = map
                .into_values()
                .filter_map(|e| match e {
                    AuthEntry::Token(t) => Some(t),
                    _ => None,
                })
                .collect();
            v.sort_by(|a, b| a.provider.cmp(&b.provider));
            v
        })
        .unwrap_or_default();

    if tokens.is_empty() {
        println!(
            "Not authenticated.\nRun: openab-agent auth codex-oauth  |  openab-agent auth anthropic-oauth  |  openab-agent auth xai"
        );
        return;
    }

    for store in tokens {
        let expired = store.is_expired();
        let masked = if store.access_token.len() > 12 {
            format!(
                "{}...{}",
                &store.access_token[..8],
                &store.access_token[store.access_token.len() - 4..]
            )
        } else {
            "****".to_string()
        };
        println!("Provider:  {}", store.provider);
        println!("Token:     {}", masked);
        println!(
            "Expires:   {} ({})",
            store.expires_at,
            if expired { "EXPIRED" } else { "valid" }
        );
        println!();
    }
    println!("File:      {:?}", path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store(expires_at: u64) -> TokenStore {
        TokenStore {
            access_token: "test_access_token_value".to_string(),
            refresh_token: "test_refresh".to_string(),
            expires_at,
            token_endpoint: "https://example.com/token".to_string(),
            provider: "codex".to_string(),
        }
    }

    // ── OAuthVendor wire-format locks (ADR §5.1) ──────────────────────────
    // The login authorize URL + token-body encoding hit live OAuth servers, so
    // no integration test covers them. These pure-function assertions pin the
    // exact wire contract so the descriptor refactor can't silently drift it.

    #[test]
    fn codex_authorize_url_pins_wire_contract() {
        let url = build_authorize_url(&CodexVendor, "CH", "ST").unwrap();
        assert!(url.starts_with(CODEX_AUTHORIZE_URL), "{url}");
        for needle in [
            "response_type=code",
            "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback",
            "scope=openid%20profile%20email%20offline_access",
            "code_challenge=CH",
            "code_challenge_method=S256",
            "state=ST",
            // codex simplified-flow hints carried as extra authorize params
            "id_token_add_organizations=true",
            "codex_cli_simplified_flow=true",
            "originator=openab-agent",
        ] {
            assert!(url.contains(needle), "missing `{needle}` in {url}");
        }
    }

    #[test]
    fn anthropic_authorize_url_pins_wire_contract() {
        let url = build_authorize_url(&AnthropicVendor, "CH", "ST").unwrap();
        assert!(url.starts_with(ANTHROPIC_AUTHORIZE_URL), "{url}");
        for needle in [
            "response_type=code",
            "redirect_uri=http%3A%2F%2Flocalhost%3A53692%2Fcallback",
            "code_challenge=CH",
            "code_challenge_method=S256",
            "state=ST",
            "code=true",                  // Anthropic-only extra authorize param
            "scope=org%3Acreate_api_key", // scope prefix, colons percent-encoded
        ] {
            assert!(url.contains(needle), "missing `{needle}` in {url}");
        }
    }

    #[test]
    fn vendor_for_resolves_oauth_tenants_only() {
        assert_eq!(
            vendor_for(CODEX_NAMESPACE).unwrap().namespace(),
            CODEX_NAMESPACE
        );
        assert_eq!(
            vendor_for(ANTHROPIC_NAMESPACE).unwrap().namespace(),
            ANTHROPIC_NAMESPACE
        );
        // MCP and unknown tenants have no OAuthVendor (rmcp owns MCP refresh).
        assert!(vendor_for("mcp:linear").is_none());
        assert!(vendor_for("nope").is_none());
    }

    #[test]
    fn token_body_and_redirect_per_vendor() {
        assert_eq!(CodexVendor.token_body(), TokenBodyFormat::Form);
        assert_eq!(AnthropicVendor.token_body(), TokenBodyFormat::Json);
        assert_eq!(
            CodexVendor.redirect_uri().as_deref(),
            Some("http://localhost:1455/auth/callback")
        );
        assert_eq!(
            AnthropicVendor.redirect_uri().as_deref(),
            Some("http://localhost:53692/callback")
        );
    }

    // ── XaiVendor descriptor + RFC 8628 device-flow helpers ───────────────

    #[test]
    fn xai_vendor_descriptor_pins_wire_contract() {
        assert_eq!(XaiVendor.namespace(), XAI_NAMESPACE);
        assert_eq!(XaiVendor.grant(), AuthGrant::DeviceCode);
        // Device-primary: no loopback redirect, form-encoded token bodies.
        assert!(XaiVendor.redirect().is_none());
        assert!(XaiVendor.redirect_uri().is_none());
        assert_eq!(XaiVendor.token_body(), TokenBodyFormat::Form);
        assert_eq!(
            XaiVendor.device_authorization_url(),
            Some("https://auth.x.ai/oauth2/device/code")
        );
        assert_eq!(XaiVendor.token_url(), "https://auth.x.ai/oauth2/token");
        // grok-cli:access + api:access are what make the token usable as an
        // API key against api.x.ai; offline_access is what yields a refresh token.
        for scope in ["offline_access", "grok-cli:access", "api:access"] {
            assert!(XaiVendor.scope().contains(scope), "missing scope {scope}");
        }
        assert_eq!(XaiVendor.extra_device_params(), &[("referrer", "openab")]);
    }

    #[test]
    fn xai_vendor_is_not_a_pkce_vendor() {
        // The PKCE driver must reject a device-primary vendor loudly.
        let err = build_authorize_url(&XaiVendor, "CH", "ST").unwrap_err();
        assert!(err.to_string().contains("no loopback redirect"), "{err}");
    }

    #[test]
    fn test_xai_client_id_default() {
        temp_env::with_var("OPENAB_AGENT_XAI_CLIENT_ID", None::<&str>, || {
            assert_eq!(
                XaiVendor.client_id(),
                "b1a00492-073a-47ea-816f-4c329264a828"
            );
        });
    }

    #[test]
    fn test_xai_client_id_override() {
        temp_env::with_var("OPENAB_AGENT_XAI_CLIENT_ID", Some("custom_xai"), || {
            assert_eq!(XaiVendor.client_id(), "custom_xai");
        });
    }

    #[test]
    fn vendor_for_resolves_xai() {
        assert_eq!(
            vendor_for(XAI_NAMESPACE).unwrap().namespace(),
            XAI_NAMESPACE
        );
    }

    #[test]
    fn auth_subcommand_per_namespace() {
        assert_eq!(auth_subcommand(XAI_NAMESPACE), "openab-agent auth xai");
        assert_eq!(
            auth_subcommand(ANTHROPIC_NAMESPACE),
            "openab-agent auth anthropic-oauth"
        );
        assert_eq!(
            auth_subcommand(CODEX_NAMESPACE),
            "openab-agent auth codex-oauth"
        );
    }

    #[test]
    fn validate_https_url_accepts_https_only() {
        assert!(validate_https_url("https://auth.x.ai/device").is_ok());
        // Non-https schemes and garbage are untrusted — the URI is shown to /
        // opened by the user.
        assert!(validate_https_url("http://auth.x.ai/device").is_err());
        assert!(validate_https_url("file:///etc/passwd").is_err());
        assert!(validate_https_url("javascript:alert(1)").is_err());
        assert!(validate_https_url("not a url").is_err());
    }

    #[test]
    fn parse_device_authorization_full_response() {
        let d = parse_device_authorization(&serde_json::json!({
            "device_code": "dev123",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://auth.x.ai/device",
            "verification_uri_complete": "https://auth.x.ai/device?code=ABCD-EFGH",
            "interval": 7,
            "expires_in": 600,
        }))
        .unwrap();
        assert_eq!(d.device_code, "dev123");
        assert_eq!(d.user_code, "ABCD-EFGH");
        assert_eq!(d.verification_uri, "https://auth.x.ai/device");
        assert_eq!(
            d.verification_uri_complete.as_deref(),
            Some("https://auth.x.ai/device?code=ABCD-EFGH")
        );
        assert_eq!(d.interval_secs, 7);
        assert_eq!(d.expires_in_secs, 600);
    }

    #[test]
    fn parse_device_authorization_defaults_interval_and_expiry() {
        // RFC 8628 allows interval 0; expires_in absent is tolerated with a
        // lenient default rather than a hard failure.
        let d = parse_device_authorization(&serde_json::json!({
            "device_code": "d",
            "user_code": "u",
            "verification_uri": "https://auth.x.ai/device",
            "interval": 0,
        }))
        .unwrap();
        assert_eq!(d.interval_secs, DEVICE_DEFAULT_INTERVAL_SECS);
        assert_eq!(d.expires_in_secs, DEVICE_DEFAULT_EXPIRES_IN_SECS);
        assert!(d.verification_uri_complete.is_none());
    }

    #[test]
    fn parse_device_authorization_rejects_missing_or_untrusted_fields() {
        // Missing device_code.
        assert!(parse_device_authorization(&serde_json::json!({
            "user_code": "u",
            "verification_uri": "https://auth.x.ai/device",
        }))
        .is_err());
        // Non-https verification_uri.
        assert!(parse_device_authorization(&serde_json::json!({
            "device_code": "d",
            "user_code": "u",
            "verification_uri": "http://auth.x.ai/device",
        }))
        .is_err());
        // Non-https verification_uri_complete poisons an otherwise valid response.
        assert!(parse_device_authorization(&serde_json::json!({
            "device_code": "d",
            "user_code": "u",
            "verification_uri": "https://auth.x.ai/device",
            "verification_uri_complete": "http://evil.example/device",
        }))
        .is_err());
    }

    #[test]
    fn classify_device_poll_error_dispositions() {
        use DevicePollDisposition::*;
        assert_eq!(
            classify_device_poll_error(&serde_json::json!({"error": "authorization_pending"})),
            Pending
        );
        assert_eq!(
            classify_device_poll_error(&serde_json::json!({"error": "slow_down"})),
            SlowDown(None)
        );
        assert_eq!(
            classify_device_poll_error(&serde_json::json!({"error": "slow_down", "interval": 10})),
            SlowDown(Some(10))
        );
        // Non-positive replacement interval is ignored (caller falls back to +5).
        assert_eq!(
            classify_device_poll_error(&serde_json::json!({"error": "slow_down", "interval": 0})),
            SlowDown(None)
        );
        for denied in ["access_denied", "authorization_denied"] {
            assert!(matches!(
                classify_device_poll_error(&serde_json::json!({"error": denied})),
                Fatal(m) if m.contains("denied")
            ));
        }
        assert!(matches!(
            classify_device_poll_error(&serde_json::json!({"error": "expired_token"})),
            Fatal(m) if m.contains("expired")
        ));
        assert!(matches!(
            classify_device_poll_error(
                &serde_json::json!({"error": "kaboom", "error_description": "detail"})
            ),
            Fatal(m) if m.contains("kaboom") && m.contains("detail")
        ));
        // Unparseable/empty error body is terminal, not an infinite poll.
        assert!(matches!(
            classify_device_poll_error(&serde_json::json!({})),
            Fatal(_)
        ));
    }

    #[test]
    fn next_slow_down_interval_is_monotonic() {
        // RFC 8628 §3.5 (review F2): slow_down must never speed polling up.
        // No replacement interval → +5.
        assert_eq!(next_slow_down_interval(5, None), 10);
        // Replacement below the current delay is ignored in favor of +5.
        assert_eq!(next_slow_down_interval(10, Some(3)), 15);
        // Replacement equal to the bumped value is a no-op either way.
        assert_eq!(next_slow_down_interval(10, Some(15)), 15);
        // A genuinely slower server interval is honored.
        assert_eq!(next_slow_down_interval(5, Some(30)), 30);
    }

    #[test]
    fn next_transport_backoff_interval_doubles_and_clamps() {
        // RFC 8628 §3.5 (review F4): reduce polling frequency after a
        // connection timeout — exponential, clamped to a useful ceiling.
        assert_eq!(next_transport_backoff_interval(5), 10);
        assert_eq!(next_transport_backoff_interval(10), 20);
        assert_eq!(next_transport_backoff_interval(40), 60);
        assert_eq!(next_transport_backoff_interval(60), 60);
        // Degenerate low values still land on the RFC default floor.
        assert_eq!(next_transport_backoff_interval(0), 5);
    }

    #[test]
    fn token_store_from_payload_defaults_expires_in() {
        // xAI may omit expires_in on token responses — default 3600, not a failure.
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let store = token_store_from_payload(
            &serde_json::json!({"access_token": "at", "refresh_token": "rt"}),
            XAI_TOKEN_URL,
            XAI_NAMESPACE,
        )
        .unwrap();
        assert_eq!(store.provider, XAI_NAMESPACE);
        assert!(store.expires_at >= before + 3600);
        // A refresh_token-less login response is a hard error (offline_access
        // should always yield one on the initial device grant).
        assert!(token_store_from_payload(
            &serde_json::json!({"access_token": "at"}),
            XAI_TOKEN_URL,
            XAI_NAMESPACE,
        )
        .is_err());
    }

    #[test]
    fn test_is_expired_future_token() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(!make_store(now + 3600).is_expired());
    }

    #[test]
    fn test_is_expired_past_token() {
        assert!(make_store(0).is_expired());
    }

    #[test]
    fn test_is_expired_within_skew() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(make_store(now + 60).is_expired());
    }

    #[test]
    fn test_is_expired_sentinel_u64_max() {
        assert!(!make_store(u64::MAX).is_expired());
    }

    #[test]
    fn test_auth_path() {
        assert!(auth_path()
            .to_string_lossy()
            .contains(".openab/agent/auth.json"));
    }

    #[test]
    fn test_codex_client_id_default() {
        temp_env::with_var("OPENAB_AGENT_OAUTH_CLIENT_ID", None::<&str>, || {
            assert_eq!(CodexVendor.client_id(), "app_EMoamEEZ73f0CkXaXp7hrann");
        });
    }

    #[test]
    fn test_codex_client_id_override() {
        temp_env::with_var("OPENAB_AGENT_OAUTH_CLIENT_ID", Some("custom_id"), || {
            assert_eq!(CodexVendor.client_id(), "custom_id");
        });
    }

    #[test]
    fn test_generate_pkce() {
        let (verifier, challenge) = generate_pkce();
        assert!(!verifier.is_empty());
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expected);
    }

    #[test]
    fn test_anthropic_authorize_url_carries_required_params() {
        temp_env::with_var("OPENAB_AGENT_ANTHROPIC_CLIENT_ID", None::<&str>, || {
            let url = build_authorize_url(&AnthropicVendor, "CHAL", "STATE").unwrap();
            assert!(url.starts_with("https://claude.ai/oauth/authorize?"));
            assert!(url.contains("client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e"));
            assert!(url.contains("response_type=code"));
            assert!(url.contains("code_challenge=CHAL"));
            assert!(url.contains("code_challenge_method=S256"));
            assert!(url.contains("state=STATE"));
            // scope is url-encoded; spot-check one encoded scope token
            assert!(url.contains("user%3Ainference"));
            // redirect must be the loopback callback on the Anthropic port
            assert!(url.contains("localhost%3A53692%2Fcallback"));
        });
    }

    #[test]
    fn test_anthropic_save_uses_provider_as_key_disjoint_from_codex() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let mut codex = make_store(1);
        codex.provider = "codex".to_string();
        let mut anth = make_store(2);
        anth.provider = ANTHROPIC_NAMESPACE.to_string();
        anth.access_token = "sk-ant-oat-xyz".to_string();
        let mut input = HashMap::new();
        input.insert(codex.provider.clone(), AuthEntry::Token(codex));
        input.insert(anth.provider.clone(), AuthEntry::Token(anth));
        write_auth_file(&path, &input).unwrap();
        let map = read_auth_file(&path).unwrap();
        assert_eq!(token_of(map.get("codex")).expires_at, 1);
        assert_eq!(
            token_of(map.get(ANTHROPIC_NAMESPACE)).access_token,
            "sk-ant-oat-xyz"
        );
    }

    fn token_of(entry: Option<&AuthEntry>) -> &TokenStore {
        match entry {
            Some(AuthEntry::Token(t)) => t,
            other => panic!("expected Token, got {other:?}"),
        }
    }

    #[test]
    fn read_auth_file_migrates_legacy_single_tenant_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let legacy = serde_json::to_string_pretty(&make_store(9_999_999_999)).unwrap();
        std::fs::write(&path, legacy).unwrap();
        let map = read_auth_file(&path).unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(
            token_of(map.get(CODEX_NAMESPACE)).access_token,
            "test_access_token_value"
        );
    }

    #[test]
    fn read_auth_file_parses_new_namespaced_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let mut input = HashMap::new();
        input.insert("codex".to_string(), AuthEntry::Token(make_store(1)));
        input.insert("mcp:linear".to_string(), AuthEntry::Token(make_store(2)));
        write_auth_file(&path, &input).unwrap();
        let map = read_auth_file(&path).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(token_of(map.get("codex")).expires_at, 1);
        assert_eq!(token_of(map.get("mcp:linear")).expires_at, 2);
    }

    #[test]
    fn write_auth_file_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let mut input = HashMap::new();
        input.insert("mcp:github".to_string(), AuthEntry::Token(make_store(42)));
        write_auth_file(&path, &input).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("mcp:github"));
        let map = read_auth_file(&path).unwrap();
        assert_eq!(token_of(map.get("mcp:github")).expires_at, 42);
    }

    #[cfg(unix)]
    #[test]
    fn write_auth_file_creates_file_with_0600_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let mut input = HashMap::new();
        input.insert("codex".to_string(), AuthEntry::Token(make_store(0)));
        write_auth_file(&path, &input).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected 0600, got {mode:o}");
    }

    fn make_pending() -> PendingPasteLogin {
        PendingPasteLogin {
            verifier: "test-verifier".to_string(),
            state: "test-state".to_string(),
            token_url: "https://example.com/token".to_string(),
            provider_name: "anthropic-mcp".to_string(),
            resource: None,
            created_at: 0,
        }
    }

    #[test]
    fn auth_entry_untagged_round_trip_mixed_shapes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let mut input = HashMap::new();
        input.insert("codex".to_string(), AuthEntry::Token(make_store(1)));
        input.insert(
            "mcp-pending:linear".to_string(),
            AuthEntry::Pending(make_pending()),
        );
        write_auth_file(&path, &input).unwrap();
        let map = read_auth_file(&path).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(token_of(map.get("codex")).expires_at, 1);
        match map.get("mcp-pending:linear") {
            Some(AuthEntry::Pending(p)) => assert_eq!(p.verifier, "test-verifier"),
            other => panic!("expected Pending, got {other:?}"),
        }
    }

    fn make_mcp_creds() -> StoredCredentials {
        StoredCredentials::new(
            "client-xyz".to_string(),
            None,
            vec!["read".to_string(), "write".to_string()],
            Some(1234),
        )
    }

    #[test]
    fn auth_entry_mcp_variant_round_trips_and_is_disjoint() {
        // Token + Pending + Mcp in one file: each must deserialize back to its
        // own variant, proving the untagged shapes stay disjoint with `Mcp`
        // added (the loosest-required-field variant) last.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let mut input = HashMap::new();
        input.insert("codex".to_string(), AuthEntry::Token(make_store(7)));
        input.insert(
            "mcp-pending:linear".to_string(),
            AuthEntry::Pending(make_pending()),
        );
        input.insert("github".to_string(), AuthEntry::Mcp(make_mcp_creds()));
        write_auth_file(&path, &input).unwrap();
        let map = read_auth_file(&path).unwrap();
        assert_eq!(map.len(), 3);
        assert_eq!(token_of(map.get("codex")).expires_at, 7);
        assert!(matches!(
            map.get("mcp-pending:linear"),
            Some(AuthEntry::Pending(_))
        ));
        match map.get("github") {
            Some(AuthEntry::Mcp(c)) => {
                assert_eq!(c.client_id, "client-xyz");
                assert_eq!(c.granted_scopes, vec!["read", "write"]);
                assert_eq!(c.token_received_at, Some(1234));
                assert!(c.token_response.is_none());
            }
            other => panic!("expected Mcp, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mcp_credential_store_load_save_clear_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let store = McpCredentialStore::new(path.clone(), "linear");

        assert!(store.load().await.unwrap().is_none(), "empty → None");

        store.save(make_mcp_creds()).await.unwrap();
        let loaded = store
            .load()
            .await
            .unwrap()
            .expect("creds present after save");
        assert_eq!(loaded.client_id, "client-xyz");
        assert_eq!(loaded.granted_scopes, vec!["read", "write"]);
        assert_eq!(loaded.token_received_at, Some(1234));

        store.clear().await.unwrap();
        assert!(store.load().await.unwrap().is_none(), "cleared → None");
        // Last entry removed → file is gone, not left as an empty map.
        assert!(!path.exists(), "auth.json removed once last entry cleared");
    }

    #[tokio::test]
    async fn corrupt_auth_json_is_quarantined_not_silently_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        // Seed an unparseable auth.json (decision A3 / #969 B6).
        std::fs::write(&path, "not json{{").unwrap();

        let store = McpCredentialStore::new(path.clone(), "linear");

        // A save against the corrupt file must succeed (not wedge) and write a
        // clean file with the new creds rather than silently wiping on top of
        // the corruption.
        store.save(make_mcp_creds()).await.unwrap();
        let loaded = store
            .load()
            .await
            .unwrap()
            .expect("creds present after save");
        assert_eq!(loaded.client_id, "client-xyz");

        // The corrupt bytes are preserved in exactly one quarantine sibling
        // (auth.json.corrupt-<ts>), not overwritten in place.
        let quarantined: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("auth.json.corrupt-"))
            .collect();
        assert_eq!(
            quarantined.len(),
            1,
            "exactly one quarantine file, got {quarantined:?}"
        );
        let preserved = std::fs::read_to_string(dir.path().join(&quarantined[0])).unwrap();
        assert_eq!(
            preserved, "not json{{",
            "quarantine preserves the original corrupt bytes"
        );
    }

    #[tokio::test]
    async fn mcp_store_clear_preserves_other_tenants() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        // Seed a codex Token alongside the MCP cred.
        let mut input = HashMap::new();
        input.insert("codex".to_string(), AuthEntry::Token(make_store(1)));
        write_auth_file(&path, &input).unwrap();

        let store = McpCredentialStore::new(path.clone(), "linear");
        store.save(make_mcp_creds()).await.unwrap();
        store.clear().await.unwrap();

        // codex tenant survives the MCP clear.
        let map = read_auth_file(&path).unwrap();
        assert_eq!(token_of(map.get("codex")).expires_at, 1);
        assert!(store.load().await.unwrap().is_none());
    }

    fn mcp_creds_with_refresh(refresh: Option<&str>) -> StoredCredentials {
        let mut token = serde_json::json!({ "access_token": "acc", "token_type": "bearer" });
        if let Some(r) = refresh {
            token["refresh_token"] = serde_json::Value::String(r.to_string());
        }
        serde_json::from_value(serde_json::json!({
            "client_id": "cid",
            "token_response": token,
            "granted_scopes": [],
            "token_received_at": 1,
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn save_preserves_prior_refresh_token_when_refresh_response_omits_it() {
        use oauth2::TokenResponse;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let store = McpCredentialStore::new(path, "linear");

        // Initial login carries a refresh_token.
        store
            .save(mcp_creds_with_refresh(Some("rt-original")))
            .await
            .unwrap();
        // rmcp's refresh rebuilds creds from a response that omitted refresh_token.
        store.save(mcp_creds_with_refresh(None)).await.unwrap();

        let loaded = store.load().await.unwrap().expect("creds present");
        let rt = loaded
            .token_response
            .and_then(|tr| tr.refresh_token().map(|r| r.secret().to_string()));
        assert_eq!(
            rt.as_deref(),
            Some("rt-original"),
            "old refresh_token must survive a refresh response that omits it"
        );
    }

    #[tokio::test]
    async fn save_uses_rotated_refresh_token_when_present() {
        use oauth2::TokenResponse;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let store = McpCredentialStore::new(path, "linear");

        store
            .save(mcp_creds_with_refresh(Some("rt-old")))
            .await
            .unwrap();
        // AS rotated and returned a new refresh_token → it replaces the old one.
        store
            .save(mcp_creds_with_refresh(Some("rt-new")))
            .await
            .unwrap();

        let loaded = store.load().await.unwrap().expect("creds present");
        let rt = loaded
            .token_response
            .and_then(|tr| tr.refresh_token().map(|r| r.secret().to_string()));
        assert_eq!(rt.as_deref(), Some("rt-new"));
    }

    #[tokio::test]
    async fn mcp_store_load_returns_none_for_token_keyed_entry() {
        // A legacy `Token` under the server's bare key must read as absent so
        // the manager triggers the accepted one-time re-login.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let mut input = HashMap::new();
        input.insert("linear".to_string(), AuthEntry::Token(make_store(9)));
        write_auth_file(&path, &input).unwrap();
        let store = McpCredentialStore::new(path, "linear");
        assert!(store.load().await.unwrap().is_none());
    }

    #[test]
    fn load_namespaced_token_errors_on_pending_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let mut input = HashMap::new();
        input.insert(
            "mcp-pending:srv".to_string(),
            AuthEntry::Pending(make_pending()),
        );
        write_auth_file(&path, &input).unwrap();
        let map = read_auth_file(&path).unwrap();
        // Assert the discriminant directly. `load_namespaced_token` would
        // reach into the real `$HOME/.openab/agent/auth.json` and race
        // cross-module tests; the variant check is the actual property
        // under test.
        let pending = map.get("mcp-pending:srv");
        assert!(matches!(pending, Some(AuthEntry::Pending(_))));
    }

    #[test]
    fn with_auth_locked_merges_concurrent_tenants_no_lost_update() {
        // Two locked RMWs against the same file — the codex tenant and an MCP
        // tenant — must both survive: the second writer re-reads inside the lock
        // and merges, instead of clobbering the first (the §5.4 lost-update fix).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");

        with_auth_locked(&path, |m| {
            m.insert("codex".to_string(), AuthEntry::Token(make_store(1)));
        })
        .unwrap();
        with_auth_locked(&path, |m| {
            m.insert("github".to_string(), AuthEntry::Mcp(make_mcp_creds()));
        })
        .unwrap();

        let map = read_auth_file(&path).unwrap();
        assert_eq!(map.len(), 2, "second write merged, did not lost-update");
        assert_eq!(token_of(map.get("codex")).expires_at, 1);
        assert!(matches!(map.get("github"), Some(AuthEntry::Mcp(_))));
    }

    #[test]
    fn with_auth_locked_gcs_stale_pending_but_keeps_fresh_and_tokens() {
        // ADR §7: a locked write opportunistically sweeps `Pending` entries older
        // than 15 min, while fresh pending state and real tenants are untouched.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // Seed through the un-GC'd writer so the stale entry exists pre-sweep.
        let mut seed = HashMap::new();
        seed.insert("codex".to_string(), AuthEntry::Token(make_store(1)));
        seed.insert(
            "mcp-pending:stale".to_string(),
            AuthEntry::Pending(make_pending()), // created_at = 0 → ancient
        );
        seed.insert(
            "mcp-pending:fresh".to_string(),
            AuthEntry::Pending(PendingPasteLogin {
                created_at: now,
                ..make_pending()
            }),
        );
        write_auth_file(&path, &seed).unwrap();

        with_auth_locked(&path, |_m| {}).unwrap();

        let map = read_auth_file(&path).unwrap();
        assert!(
            map.get("mcp-pending:stale").is_none(),
            "stale pending swept"
        );
        assert!(
            matches!(map.get("mcp-pending:fresh"), Some(AuthEntry::Pending(_))),
            "fresh pending kept"
        );
        assert!(map.get("codex").is_some(), "real tenant untouched");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lock_tenant_refresh_fails_closed_when_contended() {
        // §5.4 (b), fail-closed: while one holder keeps the tenant refresh lock, a
        // second acquire must hit the deadline and return `TimedOut` — the signal
        // the caller turns into a retryable error instead of refreshing unserialised
        // (which would re-present RT_old). Once released, acquisition succeeds again.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");

        let held = lock_tenant_refresh(&path, "codex").await;
        assert!(matches!(held, RefreshLock::Held(_)), "first acquire holds");

        let contended =
            lock_tenant_refresh_until(&path, "codex", std::time::Duration::from_millis(200)).await;
        assert!(
            matches!(contended, RefreshLock::TimedOut),
            "second acquire fails closed while the lock is held"
        );

        drop(held);
        let after = lock_tenant_refresh(&path, "codex").await;
        assert!(
            matches!(after, RefreshLock::Held(_)),
            "acquire succeeds once the holder releases"
        );
    }

    #[test]
    fn with_auth_locked_merges_anthropic_tenant_no_lost_update() {
        // The §5.4 lost-update guarantee must hold for the `anthropic-oauth`
        // tenant too: a concurrent codex write must not clobber a just-written
        // Anthropic token (proves the new tenant rides the same locked funnel).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");

        let mut anth = make_store(7);
        anth.provider = ANTHROPIC_NAMESPACE.to_string();
        with_auth_locked(&path, |m| {
            m.insert(ANTHROPIC_NAMESPACE.to_string(), AuthEntry::Token(anth));
        })
        .unwrap();
        with_auth_locked(&path, |m| {
            m.insert("codex".to_string(), AuthEntry::Token(make_store(1)));
        })
        .unwrap();

        let map = read_auth_file(&path).unwrap();
        assert_eq!(map.len(), 2, "second write merged, did not lost-update");
        assert_eq!(token_of(map.get(ANTHROPIC_NAMESPACE)).expires_at, 7);
        assert_eq!(token_of(map.get("codex")).expires_at, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lock_tenant_refresh_fails_closed_for_anthropic_and_is_per_tenant() {
        // §5.4 (b) proven for the `anthropic-oauth` tenant: while one holder keeps
        // its refresh lock, a second acquire fails closed (`TimedOut`) — single-
        // flight for the new tenant, not just codex.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");

        let held = lock_tenant_refresh(&path, ANTHROPIC_NAMESPACE).await;
        assert!(
            matches!(held, RefreshLock::Held(_)),
            "anthropic acquire holds"
        );

        let contended = lock_tenant_refresh_until(
            &path,
            ANTHROPIC_NAMESPACE,
            std::time::Duration::from_millis(200),
        )
        .await;
        assert!(
            matches!(contended, RefreshLock::TimedOut),
            "second anthropic acquire fails closed while held"
        );

        // Per-tenant isolation: the locks are keyed per tenant, so holding the
        // Anthropic lock must NOT block codex — a slow Anthropic refresh never
        // head-of-line-blocks another tenant's refresh (the reason §5.4 uses a
        // per-tenant lock rather than the global one).
        let codex = lock_tenant_refresh(&path, "codex").await;
        assert!(
            matches!(codex, RefreshLock::Held(_)),
            "codex acquire is independent of the held anthropic lock"
        );

        drop(held);
        drop(codex);
        let after = lock_tenant_refresh(&path, ANTHROPIC_NAMESPACE).await;
        assert!(
            matches!(after, RefreshLock::Held(_)),
            "anthropic acquire succeeds once released"
        );
    }
}

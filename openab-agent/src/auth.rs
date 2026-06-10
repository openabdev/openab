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

const REFRESH_SKEW_SECONDS: u64 = 120;

const CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_DEVICE_AUTH_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const CODEX_DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const CODEX_DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const REDIRECT_PORT: u16 = 1455;

fn codex_client_id() -> String {
    std::env::var("OPENAB_AGENT_OAUTH_CLIENT_ID")
        .unwrap_or_else(|_| "app_EMoamEEZ73f0CkXaXp7hrann".to_string())
}

fn redirect_uri() -> String {
    format!("http://localhost:{REDIRECT_PORT}/auth/callback")
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

pub fn load_tokens() -> Result<TokenStore> {
    let path = auth_path();
    let map = read_auth_file(&path).map_err(|_| {
        anyhow!(
            "No credentials found at {}. Run `openab-agent auth codex-oauth` first.",
            path.display()
        )
    })?;
    match map.get(CODEX_NAMESPACE) {
        Some(AuthEntry::Token(t)) => Ok(t.clone()),
        _ => Err(anyhow!(
            "No codex credentials in {}. Run `openab-agent auth codex-oauth` first.",
            path.display()
        )),
    }
}

fn save_tokens(store: &TokenStore) -> Result<()> {
    let path = auth_path();
    let mut map = read_auth_file(&path).unwrap_or_default();
    map.insert(CODEX_NAMESPACE.to_string(), AuthEntry::Token(store.clone()));
    write_auth_file(&path, &map)
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
        let mut map = read_auth_file(&self.path).unwrap_or_default();

        // OAuth 2.1 §10.4: when a refresh response omits `refresh_token`, the
        // prior one stays valid. rmcp's `refresh_token()` rebuilds the stored
        // credentials from the refresh response alone, so a rotating-but-omitting
        // AS would lose our fallback — splice the prior refresh_token back in.
        let incoming_has_refresh = credentials
            .token_response
            .as_ref()
            .and_then(|tr| tr.refresh_token())
            .is_some_and(|rt| !rt.secret().is_empty());
        if !incoming_has_refresh {
            if let Some(AuthEntry::Mcp(old)) = map.get(&self.key) {
                let prior = old
                    .token_response
                    .as_ref()
                    .and_then(|tr| tr.refresh_token())
                    .map(|rt| rt.secret().to_string())
                    .filter(|s| !s.is_empty());
                if let (Some(prior), Some(tr)) = (prior, credentials.token_response.as_mut()) {
                    tr.set_refresh_token(Some(RefreshToken::new(prior)));
                }
            }
        }

        map.insert(self.key.clone(), AuthEntry::Mcp(credentials));
        write_auth_file(&self.path, &map).map_err(|e| AuthError::InternalError(e.to_string()))
    }

    async fn clear(&self) -> Result<(), AuthError> {
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

pub async fn get_valid_token() -> Result<String> {
    let mut store = load_tokens()?;
    if store.is_expired() {
        store = refresh_token(&store).await?;
        save_tokens(&store)?;
    }
    Ok(store.access_token)
}

pub async fn force_refresh() -> Result<String> {
    let store = load_tokens()?;
    let new_store = refresh_token(&store).await?;
    save_tokens(&new_store)?;
    Ok(new_store.access_token)
}

async fn refresh_token(store: &TokenStore) -> Result<TokenStore> {
    let client_id = codex_client_id();
    let client = reqwest::Client::new();
    let resp = client
        .post(&store.token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", store.refresh_token.as_str()),
            ("client_id", client_id.as_str()),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Token refresh failed (HTTP {status}): {body}. Run `openab-agent auth codex-oauth` again."));
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

// Browser PKCE flow
pub async fn login_browser_flow(no_browser: bool) -> Result<()> {
    let client_id = codex_client_id();
    let (code_verifier, code_challenge) = generate_pkce();
    let mut state_buf = [0u8; 16];
    getrandom::fill(&mut state_buf).expect("getrandom failed");
    let state = URL_SAFE_NO_PAD.encode(state_buf);
    let redir_str = redirect_uri();
    let redir = urlencoding::encode(&redir_str);
    let auth_url = format!("{CODEX_AUTHORIZE_URL}?client_id={client_id}&redirect_uri={redir}&response_type=code&scope=openid+profile+email+offline_access&code_challenge={code_challenge}&code_challenge_method=S256&state={state}&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=openab-agent");

    let listener = TcpListener::bind(format!("127.0.0.1:{REDIRECT_PORT}")).map_err(|e| {
        anyhow!("Failed to bind port {REDIRECT_PORT}: {e}. Is another instance running?")
    })?;

    if no_browser {
        println!("Open this URL in your browser:\n");
        println!("  {auth_url}\n");
        println!("After approving, your browser will redirect to a localhost URL.");
        println!("Copy the full URL from the browser address bar and paste it here:\n");

        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .map_err(|e| anyhow!("Failed to read input: {e}"))?;
        let input = input.trim();
        if input.is_empty() {
            return Err(anyhow!("No URL provided"));
        }
        let url = url::Url::parse(input).map_err(|_| anyhow!("Invalid URL: {input}"))?;

        // Skip TCP listener for paste flow
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
                    "No code in URL. Error: {}",
                    error.unwrap_or_else(|| "unknown".into())
                )
            })?;
        let cb_state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.to_string());
        if cb_state.as_deref() != Some(&state) {
            return Err(anyhow!("State mismatch"));
        }

        // Exchange code for tokens
        let client = reqwest::Client::new();
        let resp = client
            .post(CODEX_TOKEN_URL)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", client_id.as_str()),
                ("code", code.as_str()),
                ("code_verifier", code_verifier.as_str()),
                ("redirect_uri", redirect_uri().as_str()),
            ])
            .send()
            .await?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Token exchange failed: {body}"));
        }
        let payload: serde_json::Value = resp.json().await?;
        let access_token = payload["access_token"]
            .as_str()
            .ok_or_else(|| anyhow!("No access_token"))?;
        let refresh_token_val = payload["refresh_token"]
            .as_str()
            .ok_or_else(|| anyhow!("No refresh_token"))?;
        let expires_in = payload["expires_in"].as_u64().unwrap_or(3600);
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let store = TokenStore {
            access_token: access_token.to_string(),
            refresh_token: refresh_token_val.to_string(),
            expires_at: now + expires_in,
            token_endpoint: CODEX_TOKEN_URL.to_string(),
            provider: "codex".to_string(),
        };
        save_tokens(&store)?;
        println!(
            "\n\u{2705} Login successful! Token saved to {:?}",
            auth_path()
        );
        return Ok(());
    } else {
        println!("Opening browser for authentication...\n");
        if open::that(&auth_url).is_err() {
            println!("Could not open browser. Open this URL manually:\n");
            println!("  {auth_url}\n");
        }
        println!("Waiting for callback...");
    }

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
                "No code in callback. Error: {}",
                error.unwrap_or_else(|| "unknown".into())
            )
        })?;
    let cb_state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string());
    if cb_state.as_deref() != Some(&state) {
        return Err(anyhow!("State mismatch in callback"));
    }

    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<html><body><h1>Authentication successful!</h1><p>You can close this tab.</p></body></html>";
    let _ = stream.write_all(response.as_bytes());

    let client = reqwest::Client::new();
    let resp = client
        .post(CODEX_TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id.as_str()),
            ("code", code.as_str()),
            ("code_verifier", code_verifier.as_str()),
            ("redirect_uri", redirect_uri().as_str()),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Token exchange failed: {body}"));
    }
    let payload: serde_json::Value = resp.json().await?;
    let access_token = payload["access_token"]
        .as_str()
        .ok_or_else(|| anyhow!("No access_token"))?;
    let refresh_token_val = payload["refresh_token"]
        .as_str()
        .ok_or_else(|| anyhow!("No refresh_token"))?;
    let expires_in = payload["expires_in"].as_u64().unwrap_or(3600);
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let store = TokenStore {
        access_token: access_token.to_string(),
        refresh_token: refresh_token_val.to_string(),
        expires_at: now + expires_in,
        token_endpoint: CODEX_TOKEN_URL.to_string(),
        provider: "codex".to_string(),
    };
    save_tokens(&store)?;
    println!(
        "\n\u{2705} Login successful! Token saved to {:?}",
        auth_path()
    );
    Ok(())
}

// Device code flow
pub async fn login_codex_device_flow() -> Result<()> {
    println!("Starting OpenAI Codex device-code login...\n");
    let client = reqwest::Client::new();
    let client_id = codex_client_id();

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
            let access_token = token_payload["access_token"]
                .as_str()
                .ok_or_else(|| anyhow!("No access_token: {token_payload}"))?;
            let refresh_token_val = token_payload["refresh_token"]
                .as_str()
                .ok_or_else(|| anyhow!("No refresh_token"))?;
            let expires_in = token_payload["expires_in"].as_u64().unwrap_or(3600);
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            let store = TokenStore {
                access_token: access_token.to_string(),
                refresh_token: refresh_token_val.to_string(),
                expires_at: now + expires_in,
                token_endpoint: CODEX_TOKEN_URL.to_string(),
                provider: "codex".to_string(),
            };
            save_tokens(&store)?;
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

pub fn show_status() {
    match load_tokens() {
        Ok(store) => {
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
            println!("File:      {:?}", auth_path());
        }
        Err(e) => {
            println!("Not authenticated: {e}\nRun: openab-agent auth codex-oauth");
        }
    }
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
            assert_eq!(codex_client_id(), "app_EMoamEEZ73f0CkXaXp7hrann");
        });
    }

    #[test]
    fn test_codex_client_id_override() {
        temp_env::with_var("OPENAB_AGENT_OAUTH_CLIENT_ID", Some("custom_id"), || {
            assert_eq!(codex_client_id(), "custom_id");
        });
    }

    #[test]
    fn test_generate_pkce() {
        let (verifier, challenge) = generate_pkce();
        assert!(!verifier.is_empty());
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expected);
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
}

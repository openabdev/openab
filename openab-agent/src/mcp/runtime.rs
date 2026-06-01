//! Per-server lifecycle manager. See ADR §5.4 + §5.7.
//!
//! Handles live behind `Arc<tokio::sync::RwLock<...>>` so `connect()` (async,
//! spawns child processes) is `Send` across `.await` and a background idle-
//! eviction task can share the map with foreground `mcp call` invocations
//! (ADR §5.7). Read-heavy / write-light fits `RwLock`.
//!
//! `connect()` uses a double-lock pattern: a short write lock to mark
//! `Connecting`, release the lock, run the rmcp handshake without holding
//! any lock, then re-acquire briefly to install the client or record the
//! failure. Holding the write lock across the `serve(...).await` would
//! starve every reader (including `mcp status` and the eviction scan) for
//! the duration of a child-process spawn + handshake.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess};
use rmcp::ServiceExt;
use tokio::process::Command;
use tokio::sync::RwLock;

use super::config::{McpConfig, ServerConfig};
use super::flow::{init_paste_authorize, parse_paste_callback};
use super::oauth::{builtin_client_id, resolve, ResolvedProvider};
use crate::auth::{
    auth_path, is_expired, list_pending_logins_at, load_namespaced_token_at, load_pending_login,
    pending_key, remove_pending_login, save_namespaced_token_at, save_pending_login,
    PendingPasteLogin, TokenStore,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerStatus {
    Disconnected,
    Connecting,
    Connected,
    NeedsAuth,
    Failed(String),
}

impl ServerStatus {
    pub fn icon(&self) -> &'static str {
        match self {
            ServerStatus::Disconnected => "○",
            ServerStatus::Connecting => "◐",
            ServerStatus::Connected => "●",
            ServerStatus::NeedsAuth => "◌",
            ServerStatus::Failed(_) => "✗",
        }
    }
}

pub struct ServerHandle {
    pub name: String,
    pub config: ServerConfig,
    pub status: ServerStatus,
    /// `Arc` so foreground callers can clone a peer handle out under a
    /// short read lock, drop the guard, and then run `peer.list_all_tools()`
    /// / `peer.call_tool()` without holding any runtime lock across the
    /// I/O `.await` (avoids writer starvation + `Future is not Send` traps).
    pub client: Option<Arc<RunningService<RoleClient, ()>>>,
}

impl std::fmt::Debug for ServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerHandle")
            .field("name", &self.name)
            .field("config", &self.config)
            .field("status", &self.status)
            .field("client", &self.client.is_some())
            .finish()
    }
}

/// Public return of `start_paste_login`. The caller relays `authorize_url`
/// to the user; `state` is echoed so the agent can show / log it without
/// reaching into runtime internals.
#[derive(Debug, Clone)]
pub struct PasteLoginStart {
    pub authorize_url: String,
    pub state: String,
}

/// Owns one `ServerHandle` per configured server, behind an async `RwLock`
/// so the foreground LLM path and the background eviction task can share it.
#[derive(Debug, Clone)]
pub struct McpRuntimeManager {
    handles: Arc<RwLock<HashMap<String, ServerHandle>>>,
    /// `auth.json` location used for `mcp-pending:<server>` persistence.
    /// Injectable so tests can point at a tempdir instead of `$HOME`,
    /// avoiding cross-module HOME-env races (Tick 24 lesson + ADR §6.4).
    auth_path: PathBuf,
}

impl McpRuntimeManager {
    pub fn from_config(cfg: McpConfig) -> Self {
        Self::from_config_with_auth_path(cfg, auth_path())
    }

    pub fn from_config_with_auth_path(cfg: McpConfig, auth_path: PathBuf) -> Self {
        let handles: HashMap<_, _> = cfg
            .servers
            .into_iter()
            .map(|(name, config)| {
                let handle = ServerHandle {
                    name: name.clone(),
                    config,
                    status: ServerStatus::Disconnected,
                    client: None,
                };
                (name, handle)
            })
            .collect();
        Self {
            handles: Arc::new(RwLock::new(handles)),
            auth_path,
        }
    }

    /// Snapshot of `(name, status)` sorted by name. Clones out so the read
    /// guard is dropped before returning — callers don't hold a lock.
    pub async fn statuses(&self) -> Vec<(String, ServerStatus)> {
        let mut out: Vec<_> = {
            let guard = self.handles.read().await;
            guard
                .iter()
                .map(|(name, h)| (name.clone(), h.status.clone()))
                .collect()
        };
        out.sort_by(|(a, _), (b, _)| a.cmp(b));
        out
    }

    pub async fn is_empty(&self) -> bool {
        self.handles.read().await.is_empty()
    }

    /// Sorted server names with an in-flight `mcp-pending:<name>` entry in
    /// `auth.json`. Lets `mcp status` surface "you started a login but
    /// haven't finished" — including for servers no longer in config
    /// (caller cross-references against `statuses()` to spot orphans).
    pub fn pending_logins(&self) -> Vec<String> {
        list_pending_logins_at(&self.auth_path)
    }

    /// Clone the live MCP client handle for `name` out from under a short
    /// read lock. The caller `.await`s on the returned `Arc` with no
    /// runtime lock held, so background writers (idle eviction, new
    /// `connect`s) are not starved by long-running tool calls.
    ///
    /// Errors if the server isn't configured or isn't currently
    /// `Connected`. Callers that want lazy-connect should run
    /// `connect(name)` first.
    pub async fn arc_peer(&self, name: &str) -> Result<Arc<RunningService<RoleClient, ()>>> {
        let guard = self.handles.read().await;
        let handle = guard
            .get(name)
            .ok_or_else(|| anyhow!("no mcp server named {name:?}"))?;
        handle
            .client
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("mcp server {name:?} is not connected"))
    }

    /// Snapshot of `(name, status, transport_label)` sorted by name. Used
    /// by the `list_servers` meta-tool action; the static transport label
    /// avoids cloning the `Stdio { args, env, .. }` payload.
    pub async fn snapshot(&self) -> Vec<(String, ServerStatus, &'static str)> {
        let mut out: Vec<_> = {
            let guard = self.handles.read().await;
            guard
                .iter()
                .map(|(name, h)| (name.clone(), h.status.clone(), h.config.transport_label()))
                .collect()
        };
        out.sort_by(|(a, ..), (b, ..)| a.cmp(b));
        out
    }

    /// Begin a paste-back OAuth login for an HTTP server with an `oauth:`
    /// block (ADR §6.4). Produces the authorize URL the agent surfaces to
    /// the user; the matching PKCE verifier + `state` nonce are persisted
    /// under `mcp-pending:<name>` in `auth.json` for `complete_login`
    /// (next slice) to consume.
    ///
    /// Scoped to **built-in** providers this slice. Custom-provider
    /// paste-back needs runtime port allocation for the callback (§6.4),
    /// and any provider that advertises a `device_authorization_endpoint`
    /// should run device-code instead (§6.4 selection logic). Both errors
    /// are explicit so the LLM can pick a different action.
    pub async fn start_paste_login(&self, name: &str) -> Result<PasteLoginStart> {
        let (provider, client_id, redirect_uri) = self.resolve_paste_client(name).await?;
        let started = init_paste_authorize(&provider, &client_id, &redirect_uri)?;
        let pending = PendingPasteLogin {
            verifier: started.code_verifier,
            state: started.state.clone(),
            token_url: provider.token_url().to_string(),
            provider_name: provider_name_of(&provider),
        };
        save_pending_login(&self.auth_path, &pending_key(name), &pending)?;
        {
            let mut handles = self.handles.write().await;
            if let Some(handle) = handles.get_mut(name) {
                handle.status = ServerStatus::NeedsAuth;
            }
        }
        Ok(PasteLoginStart {
            authorize_url: started.url,
            state: started.state,
        })
    }

    /// Read the on-disk pending paste-login for `name`. `None` if there's
    /// no entry or the file is unreadable. Used by `complete_login` to
    /// drive flow continuation and by `mcp status` to surface a partially
    /// completed login (next slice will add the status surfacing).
    #[allow(dead_code)] // wired in next slice (mcp status surfacing)
    pub async fn pending_paste_login(&self, name: &str) -> Option<PendingPasteLogin> {
        load_pending_login(&self.auth_path, &pending_key(name)).ok()
    }

    /// Finish a paste-back OAuth flow (ADR §6.4). Reads the snapshotted
    /// `PendingPasteLogin`, validates the redirect URL's `state` against
    /// the snapshotted nonce (RFC 6749 §10.12), exchanges the auth code
    /// at the snapshotted `token_url`, persists the resulting
    /// `TokenStore` under `<name>`, and clears the pending entry. Status
    /// transitions `NeedsAuth → Disconnected` so the next `connect()`
    /// dials the now-authenticated transport.
    pub async fn complete_login(&self, name: &str, redirect_url: &str) -> Result<()> {
        let pending = load_pending_login(&self.auth_path, &pending_key(name))
            .map_err(|_| anyhow!("no pending login for {name:?}; run `mcp login {name}` first"))?;
        let code = parse_paste_callback(redirect_url, &pending.state)?;
        let (_provider, client_id, redirect_uri) = self.resolve_paste_client(name).await?;
        let resp = post_token_exchange(
            &pending.token_url,
            &client_id,
            &redirect_uri,
            &code,
            &pending.verifier,
        )
        .await?;
        self.finish_login(name, &pending, resp).await
    }

    /// Pure-persistence tail of `complete_login`. Split out so tests can
    /// drive the state-machine + on-disk transition without a real token
    /// endpoint. Errors leave the pending entry intact so the user can
    /// retry the same flow.
    async fn finish_login(
        &self,
        name: &str,
        pending: &PendingPasteLogin,
        resp: TokenExchangeResponse,
    ) -> Result<()> {
        // `expires_in: None` means the provider didn't advertise a
        // lifetime (Figma, Sentry, xAI as of writing). Falling back to
        // `now + 0` (Mira's Tick 46 catch) would set the token "already
        // expired", triggering an immediate refresh on the next
        // connect() — which fails closed if refresh_token is also None,
        // bouncing the user back to NeedsAuth seconds after a successful
        // login. Treat absent `expires_in` as a long-lived token via the
        // u64::MAX sentinel: `is_expired` will return false until the
        // provider eventually 401s on use (at which point the user runs
        // `mcp login` again, the correct UX for non-refreshable tokens).
        let expires_at = match resp.expires_in {
            Some(secs) => now_secs().saturating_add(secs),
            None => u64::MAX,
        };
        let store = TokenStore {
            access_token: resp.access_token,
            refresh_token: resp.refresh_token.unwrap_or_default(),
            expires_at,
            token_endpoint: pending.token_url.clone(),
            provider: pending.provider_name.clone(),
        };
        save_namespaced_token_at(&self.auth_path, name, &store)?;
        remove_pending_login(&self.auth_path, &pending_key(name))?;
        let mut handles = self.handles.write().await;
        if let Some(handle) = handles.get_mut(name) {
            handle.status = ServerStatus::Disconnected;
        }
        Ok(())
    }

    /// Resolve a paste-back OAuth client `(provider, client_id, redirect_uri)`
    /// from the server's config. Shared by `start_paste_login` and
    /// `complete_login` so a config drift between init and finish surfaces
    /// the same error from both entry points.
    async fn resolve_paste_client(&self, name: &str) -> Result<(ResolvedProvider, String, String)> {
        let oauth_cfg = {
            let guard = self.handles.read().await;
            let handle = guard
                .get(name)
                .ok_or_else(|| anyhow!("no mcp server named {name:?}"))?;
            match handle.config.resolved(name)? {
                ServerConfig::Http {
                    oauth: Some(oauth), ..
                } => oauth,
                ServerConfig::Http { oauth: None, .. } => {
                    return Err(anyhow!("mcp server {name:?} has no oauth block"));
                }
                ServerConfig::Stdio { .. } => {
                    return Err(anyhow!("mcp server {name:?} is stdio, not http+oauth"));
                }
            }
        };
        let provider = resolve(&oauth_cfg)?;
        let (client_id, redirect_uri) = match &provider {
            ResolvedProvider::Builtin {
                provider_name,
                callback,
                ..
            } => (builtin_client_id(provider_name)?, (*callback).to_string()),
            ResolvedProvider::Custom {
                device_authorization_endpoint: Some(_),
                ..
            } => {
                return Err(anyhow!(
                    "mcp server {name:?} has a device endpoint; use device flow"
                ));
            }
            ResolvedProvider::Custom { .. } => {
                return Err(anyhow!(
                    "mcp server {name:?}: custom-provider paste-back not yet supported"
                ));
            }
        };
        Ok((provider, client_id, redirect_uri))
    }

    /// RFC 6749 §6 refresh-grant — exchange a cached `refresh_token` for a
    /// new `access_token`. Resolves `client_id` from current config (so a
    /// rotated builtin catalog entry is picked up automatically). Per
    /// ADR §6.6 rotation contract: if the provider omits a new
    /// `refresh_token` in the response, the previous one is preserved
    /// (Google-style rotation); the agent fsyncs `auth.json` before
    /// returning so deployment-side mtime watchers can sync the rotated
    /// token to peer replicas.
    async fn try_refresh_oauth_token(&self, name: &str, store: &TokenStore) -> Result<TokenStore> {
        if store.refresh_token.is_empty() {
            return Err(anyhow!("no refresh_token cached for {name:?}"));
        }
        let (_provider, client_id, _redirect_uri) = self.resolve_paste_client(name).await?;
        let resp =
            post_token_refresh(&store.token_endpoint, &client_id, &store.refresh_token).await?;
        let new_refresh = resp
            .refresh_token
            .unwrap_or_else(|| store.refresh_token.clone());
        let expires_at = match resp.expires_in {
            Some(secs) => now_secs() + secs,
            None => u64::MAX,
        };
        let new_store = TokenStore {
            access_token: resp.access_token,
            refresh_token: new_refresh,
            expires_at,
            token_endpoint: store.token_endpoint.clone(),
            provider: store.provider.clone(),
        };
        save_namespaced_token_at(&self.auth_path, name, &new_store)?;
        Ok(new_store)
    }

    /// Lazy-connect the named server (ADR §5.7). Idempotent if already
    /// `Connected` with a live client. HTTP servers with an `oauth:` block
    /// are routed through `mcp login` first — `connect` marks them
    /// `NeedsAuth` and returns an error pointing the caller at the login
    /// subcommand rather than attempting an unauthenticated dial.
    pub async fn connect(&self, name: &str) -> Result<()> {
        let plan = {
            let mut guard = self.handles.write().await;
            let handle = guard
                .get_mut(name)
                .ok_or_else(|| anyhow!("no mcp server named {name:?}"))?;
            if matches!(handle.status, ServerStatus::Connected) && handle.client.is_some() {
                return Ok(());
            }
            let resolved = handle.config.resolved(name)?;
            let plan = match resolved {
                ServerConfig::Stdio {
                    command, args, env, ..
                } => DialPlan::Dial(Dial::Stdio { command, args, env }),
                // Oauth-protected: cached-valid → dial; expired but with a
                // refresh_token → defer to outside-lock async refresh
                // (`DialPlan::NeedsRefresh`); missing/expired-no-refresh
                // → bounce to `NeedsAuth` so `mcp login` stays the user-
                // actionable path.
                ServerConfig::Http {
                    url,
                    oauth: Some(_),
                    ..
                } => match load_namespaced_token_at(&self.auth_path, name) {
                    Ok(store) if !is_expired(&store) => DialPlan::Dial(Dial::Http {
                        url,
                        auth: Some(store.access_token),
                    }),
                    Ok(store) if !store.refresh_token.is_empty() => {
                        DialPlan::NeedsRefresh { url, store }
                    }
                    _ => {
                        handle.status = ServerStatus::NeedsAuth;
                        return Err(anyhow!(
                            "mcp server {name:?} needs oauth login — run `mcp login {name}`"
                        ));
                    }
                },
                ServerConfig::Http { url, .. } => DialPlan::Dial(Dial::Http { url, auth: None }),
            };
            handle.status = ServerStatus::Connecting;
            plan
        };

        // Resolve `NeedsRefresh` outside the write lock so a slow refresh
        // doesn't block concurrent `mcp status` reads. Failed refresh →
        // `NeedsAuth` (matching the missing-token bounce inside the lock).
        let dial = match plan {
            DialPlan::Dial(d) => d,
            DialPlan::NeedsRefresh { url, store } => {
                match self.try_refresh_oauth_token(name, &store).await {
                    Ok(new_store) => Dial::Http {
                        url,
                        auth: Some(new_store.access_token),
                    },
                    Err(e) => {
                        let mut guard = self.handles.write().await;
                        if let Some(h) = guard.get_mut(name) {
                            h.status = ServerStatus::NeedsAuth;
                        }
                        return Err(anyhow!(
                            "mcp server {name:?} oauth refresh failed: {e:#} — run `mcp login {name}`"
                        ));
                    }
                }
            }
        };

        let dial_result = dial.run().await;

        let mut guard = self.handles.write().await;
        let handle = guard
            .get_mut(name)
            .ok_or_else(|| anyhow!("server {name:?} vanished during connect"))?;
        // Race guard: a concurrent connect() may have installed a client while
        // we were dialing. Yield to the winner — `dial_result` drops here,
        // killing the duplicate child via RunningService's Drop impl.
        if matches!(handle.status, ServerStatus::Connected) && handle.client.is_some() {
            return Ok(());
        }
        match dial_result {
            Ok(client) => {
                handle.status = ServerStatus::Connected;
                handle.client = Some(Arc::new(client));
                Ok(())
            }
            Err(e) => {
                let msg = format!("{e:#}");
                handle.status = ServerStatus::Failed(msg.clone());
                Err(anyhow!(msg))
            }
        }
    }
}

/// Stringified provider name for the pending-state record. `Builtin` keeps
/// its `&'static str` static; `Custom` already owns a `String`.
fn provider_name_of(provider: &ResolvedProvider) -> String {
    match provider {
        ResolvedProvider::Builtin { provider_name, .. } => (*provider_name).to_string(),
        ResolvedProvider::Custom { provider_name, .. } => provider_name.clone(),
    }
}

/// Wall-clock seconds since Unix epoch. Saturates at 0 if the clock is
/// pre-epoch (would only happen on a misconfigured container).
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Token endpoint response (RFC 6749 §4.1.4 / §5.1). `refresh_token` and
/// `expires_in` are optional — some providers (xAI as of writing) omit
/// them on initial exchange. The runtime tolerates the absence and
/// records empty/zero, leaving the refresh path to bail explicitly when
/// invoked.
#[derive(Debug, serde::Deserialize)]
struct TokenExchangeResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// POST the auth code to the OAuth 2.1 token endpoint per RFC 6749
/// §4.1.3 + RFC 7636 §4.5 (PKCE verifier). Public client — no
/// `client_secret`. Errors fold body text into the message so transient
/// 4xx from the provider land in the user's terminal verbatim.
async fn post_token_exchange(
    token_url: &str,
    client_id: &str,
    redirect_uri: &str,
    code: &str,
    code_verifier: &str,
) -> Result<TokenExchangeResponse> {
    let client = reqwest::Client::builder()
        .build()
        .context("build reqwest client")?;
    let resp = client
        .post(token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", code_verifier),
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
        ])
        .send()
        .await
        .with_context(|| format!("POST {token_url} (token exchange)"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("token endpoint returned {status}: {body}"));
    }
    serde_json::from_str(&body).map_err(|e| anyhow!("invalid token response: {e}; body={body}"))
}

/// POST a refresh-grant to the OAuth 2.1 token endpoint per RFC 6749 §6.
/// Public client — no `client_secret`. Same response shape as the
/// auth-code exchange (`TokenExchangeResponse`).
async fn post_token_refresh(
    token_url: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenExchangeResponse> {
    let client = reqwest::Client::builder()
        .build()
        .context("build reqwest client")?;
    let resp = client
        .post(token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send()
        .await
        .with_context(|| format!("POST {token_url} (token refresh)"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("token endpoint returned {status}: {body}"));
    }
    serde_json::from_str(&body).map_err(|e| anyhow!("invalid token response: {e}; body={body}"))
}

/// Two-phase plan for `connect()`: most server types resolve directly to
/// a `Dial`, but HTTP+oauth with an expired-but-refreshable token needs
/// async work (the refresh POST) before a `Dial` can be built. Keeping
/// the variant lets us release the write lock before the refresh.
enum DialPlan {
    Dial(Dial),
    NeedsRefresh { url: String, store: TokenStore },
}

/// Per-transport dial parameters, extracted under the manager's write lock
/// then dialed without holding the lock. Flat (no nested `*Dial` structs)
/// because two variants don't warrant a dispatch enum.
enum Dial {
    Stdio {
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        /// Bearer token for oauth-protected servers; `None` for anonymous HTTP.
        auth: Option<String>,
    },
}

impl Dial {
    async fn run(self) -> Result<RunningService<RoleClient, ()>> {
        match self {
            Dial::Stdio { command, args, env } => {
                let cmd = Command::new(&command).configure(|c| {
                    c.env_clear();
                    c.envs(stdio_child_env(&env));
                    c.args(&args);
                });
                let transport = TokioChildProcess::new(cmd)
                    .with_context(|| format!("spawn mcp child process {command:?}"))?;
                ().serve(transport)
                    .await
                    .with_context(|| format!("mcp handshake with {command:?}"))
            }
            Dial::Http { url, auth } => {
                let transport = match auth {
                    Some(token) => {
                        let cfg = StreamableHttpClientTransportConfig::with_uri(url.as_str())
                            .auth_header(token);
                        StreamableHttpClientTransport::from_config(cfg)
                    }
                    None => StreamableHttpClientTransport::from_uri(url.as_str()),
                };
                ().serve(transport)
                    .await
                    .with_context(|| format!("mcp handshake with {url:?}"))
            }
        }
    }
}

fn stdio_child_env(explicit: &HashMap<String, String>) -> HashMap<String, String> {
    let mut env = baseline_child_env();
    env.extend(explicit.clone());
    env
}

fn baseline_child_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    for key in baseline_env_keys() {
        if let Ok(val) = std::env::var(key) {
            env.insert((*key).to_string(), val);
        }
    }
    env
}

#[cfg(unix)]
fn baseline_env_keys() -> &'static [&'static str] {
    &["HOME", "PATH", "TERM", "USER"]
}

#[cfg(windows)]
fn baseline_env_keys() -> &'static [&'static str] {
    &[
        "HOME",
        "PATH",
        "TERM",
        "USERPROFILE",
        "USERNAME",
        "SystemRoot",
        "SystemDrive",
    ]
}

#[cfg(not(any(unix, windows)))]
fn baseline_env_keys() -> &'static [&'static str] {
    &["HOME", "PATH", "TERM"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn from_config_initializes_each_server_disconnected() {
        let json = r#"{
            "mcpServers": {
                "fs": { "type": "stdio", "command": "mcp-server-filesystem" },
                "linear": { "type": "http", "url": "https://mcp.linear.app/mcp" }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        let statuses = mgr.statuses().await;
        assert_eq!(statuses.len(), 2);
        for (_, status) in statuses {
            assert_eq!(status, ServerStatus::Disconnected);
        }
    }

    #[tokio::test]
    async fn empty_config_yields_empty_manager() {
        let mgr = McpRuntimeManager::from_config(McpConfig::default());
        assert!(mgr.is_empty().await);
        assert!(mgr.statuses().await.is_empty());
    }

    #[tokio::test]
    async fn statuses_sorted_by_name() {
        let json = r#"{
            "mcpServers": {
                "zed": { "type": "stdio", "command": "z" },
                "alpha": { "type": "stdio", "command": "a" },
                "mid": { "type": "stdio", "command": "m" }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        let names: Vec<String> = mgr.statuses().await.into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["alpha", "mid", "zed"]);
    }

    #[tokio::test]
    async fn connect_unknown_server_errors() {
        let mgr = McpRuntimeManager::from_config(McpConfig::default());
        let err = mgr.connect("missing").await.unwrap_err().to_string();
        assert!(err.contains("missing"), "expected 'missing' in {err}");
    }

    #[tokio::test]
    async fn connect_http_with_oauth_marks_needs_auth() {
        let json = r#"{
            "mcpServers": {
                "linear": {
                    "type": "http",
                    "url": "https://mcp.linear.app/mcp",
                    "oauth": { "provider": "linear" }
                }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        let err = mgr.connect("linear").await.unwrap_err().to_string();
        assert!(err.contains("needs oauth login"), "expected hint in {err}");
        assert!(
            err.contains("mcp login"),
            "expected 'mcp login' hint in {err}"
        );
        assert_eq!(mgr.statuses().await[0].1, ServerStatus::NeedsAuth);
    }

    #[tokio::test]
    async fn connect_oauth_twice_keeps_needs_auth_sticky() {
        // Second connect() must NOT silently re-enter `Connecting` and
        // shadow the user-actionable state — the only path out of
        // `NeedsAuth` is a successful `mcp login`.
        let json = r#"{
            "mcpServers": {
                "linear": {
                    "type": "http",
                    "url": "https://mcp.linear.app/mcp",
                    "oauth": { "provider": "linear" }
                }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        assert!(mgr.connect("linear").await.is_err());
        assert!(mgr.connect("linear").await.is_err());
        assert_eq!(mgr.statuses().await[0].1, ServerStatus::NeedsAuth);
    }

    #[tokio::test]
    async fn connect_http_anonymous_to_dead_address_records_failed() {
        // 127.0.0.1:1 is a TCP port that no MCP server will ever bind. The
        // handshake `.serve()` future fails fast at the connect() syscall,
        // so this test stays hermetic — no network reachability assumed.
        let json = r#"{
            "mcpServers": {
                "dead": { "type": "http", "url": "http://127.0.0.1:1/mcp" }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        let err = mgr.connect("dead").await.unwrap_err().to_string();
        assert!(err.contains("handshake"), "expected 'handshake' in {err}");
        match &mgr.statuses().await[0].1 {
            ServerStatus::Failed(_) => {}
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    // start_paste_login + builtin_client_id race on the same env var.
    // Same fix as oauth.rs / acp.rs (Tick 24 lesson).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn linear_custom_cfg() -> &'static str {
        r#"{
            "mcpServers": {
                "linear": {
                    "type": "http",
                    "url": "https://mcp.linear.app/mcp",
                    "oauth": {
                        "provider": "linear",
                        "authorize_url": "https://linear.app/oauth/authorize",
                        "token_url": "https://api.linear.app/oauth/token",
                        "client_id": "linear-client",
                        "scopes": ["read"]
                    }
                }
            }
        }"#
    }

    fn anthropic_builtin_cfg() -> &'static str {
        r#"{
            "mcpServers": {
                "anthro": {
                    "type": "http",
                    "url": "https://example.com/mcp",
                    "oauth": { "provider": "anthropic-mcp" }
                }
            }
        }"#
    }

    async fn start_login_err(mgr: &McpRuntimeManager, name: &str) -> String {
        mgr.start_paste_login(name).await.unwrap_err().to_string()
    }

    fn mgr_with_tempdir(cfg: McpConfig) -> (McpRuntimeManager, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = McpRuntimeManager::from_config_with_auth_path(cfg, dir.path().join("auth.json"));
        (mgr, dir)
    }

    #[tokio::test]
    async fn start_paste_login_builtin_returns_authorize_url_and_pins_pending() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: serialized via ENV_LOCK; isolated env key.
        unsafe {
            std::env::set_var("OPENAB_MCP_ANTHROPIC_CLIENT_ID", "anth-cid");
        }
        let cfg: McpConfig = serde_json::from_str(anthropic_builtin_cfg()).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        let start = mgr.start_paste_login("anthro").await.unwrap();
        assert!(start
            .authorize_url
            .starts_with("https://claude.ai/oauth/authorize?"));
        assert!(start.authorize_url.contains("client_id=anth-cid"));
        assert!(start
            .authorize_url
            .contains(&format!("state={}", start.state)));
        let pending = mgr.pending_paste_login("anthro").await.unwrap();
        assert_eq!(pending.state, start.state);
        assert!(!pending.verifier.is_empty());
        assert_eq!(
            pending.token_url,
            "https://platform.claude.com/v1/oauth/token"
        );
        assert_eq!(pending.provider_name, "anthropic-mcp");
        assert_eq!(mgr.statuses().await[0].1, ServerStatus::NeedsAuth);
        unsafe {
            std::env::remove_var("OPENAB_MCP_ANTHROPIC_CLIENT_ID");
        }
    }

    #[tokio::test]
    async fn start_paste_login_rejects_custom_provider_for_now() {
        let cfg: McpConfig = serde_json::from_str(linear_custom_cfg()).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        let err = start_login_err(&mgr, "linear").await;
        assert!(err.contains("custom-provider"), "got: {err}");
        assert!(mgr.pending_paste_login("linear").await.is_none());
    }

    #[tokio::test]
    async fn start_paste_login_rejects_custom_with_device_endpoint() {
        let json = r#"{
            "mcpServers": {
                "dev": {
                    "type": "http",
                    "url": "https://example.com/mcp",
                    "oauth": {
                        "provider": "dev",
                        "authorize_url": "https://example.com/oauth/authorize",
                        "token_url": "https://example.com/oauth/token",
                        "device_authorization_endpoint": "https://example.com/oauth/device"
                    }
                }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        let err = start_login_err(&mgr, "dev").await;
        assert!(err.contains("device flow"), "got: {err}");
    }

    #[tokio::test]
    async fn start_paste_login_rejects_stdio_server() {
        let json = r#"{
            "mcpServers": {
                "fs": { "type": "stdio", "command": "mcp-server-filesystem" }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        let err = start_login_err(&mgr, "fs").await;
        assert!(err.contains("stdio"), "got: {err}");
    }

    #[tokio::test]
    async fn start_paste_login_unknown_server_errors() {
        let mgr = McpRuntimeManager::from_config(McpConfig::default());
        let err = start_login_err(&mgr, "ghost").await;
        assert!(err.contains("ghost"), "got: {err}");
    }

    #[tokio::test]
    async fn start_paste_login_builtin_without_env_var_errors_loud() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("OPENAB_MCP_ANTHROPIC_CLIENT_ID");
        }
        let cfg: McpConfig = serde_json::from_str(anthropic_builtin_cfg()).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        let err = start_login_err(&mgr, "anthro").await;
        assert!(err.contains("OPENAB_MCP_ANTHROPIC_CLIENT_ID"), "got: {err}");
    }

    #[tokio::test]
    async fn connect_to_missing_binary_records_failed() {
        let json = r#"{
            "mcpServers": {
                "broken": {
                    "type": "stdio",
                    "command": "/nonexistent/path/openab-mcp-test-stub-zzz"
                }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        let err = mgr.connect("broken").await.unwrap_err().to_string();
        assert!(err.contains("spawn"), "expected 'spawn' in {err}");
        match &mgr.statuses().await[0].1 {
            ServerStatus::Failed(msg) => assert!(msg.contains("spawn")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn stdio_child_env_keeps_only_baseline_plus_explicit() {
        let mut explicit = HashMap::new();
        explicit.insert("MCP_TOKEN".to_string(), "server-token".to_string());
        explicit.insert("PATH".to_string(), "/custom/bin".to_string());

        let env = stdio_child_env(&explicit);

        assert_eq!(
            env.get("MCP_TOKEN").map(String::as_str),
            Some("server-token")
        );
        assert_eq!(env.get("PATH").map(String::as_str), Some("/custom/bin"));
        assert!(!env.contains_key("DISCORD_BOT_TOKEN"));
        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
    }

    fn seed_pending(mgr: &McpRuntimeManager, name: &str, state: &str) -> PendingPasteLogin {
        let pending = PendingPasteLogin {
            verifier: "v3rifier".to_string(),
            state: state.to_string(),
            token_url: "https://example.test/token".to_string(),
            provider_name: "linear".to_string(),
        };
        save_pending_login(&mgr.auth_path, &pending_key(name), &pending).unwrap();
        pending
    }

    #[tokio::test]
    async fn complete_login_rejects_when_no_pending_entry() {
        let cfg: McpConfig = serde_json::from_str(linear_custom_cfg()).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        let err = mgr
            .complete_login("linear", "http://localhost/cb?code=c&state=s")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("no pending login"), "expected hint in {err}");
        assert!(err.contains("mcp login"), "expected CLI hint in {err}");
    }

    #[tokio::test]
    async fn complete_login_rejects_state_mismatch_and_keeps_pending() {
        let cfg: McpConfig = serde_json::from_str(linear_custom_cfg()).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        let pending = seed_pending(&mgr, "linear", "want");
        let url = "http://localhost/cb?code=c&state=other";
        let err = mgr
            .complete_login("linear", url)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("state mismatch"),
            "expected CSRF guard in {err}"
        );
        // Pending entry must survive a rejected attempt so the user can
        // re-issue the same paste without going through `mcp login` again.
        let got = mgr.pending_paste_login("linear").await.unwrap();
        assert_eq!(got, pending);
    }

    #[tokio::test]
    async fn finish_login_persists_token_clears_pending_and_unblocks_connect() {
        let cfg: McpConfig = serde_json::from_str(linear_custom_cfg()).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        let pending = seed_pending(&mgr, "linear", "s");
        // Pre-set NeedsAuth so we can observe the transition.
        {
            let mut h = mgr.handles.write().await;
            h.get_mut("linear").unwrap().status = ServerStatus::NeedsAuth;
        }
        let resp = TokenExchangeResponse {
            access_token: "atok".to_string(),
            refresh_token: Some("rtok".to_string()),
            expires_in: Some(3600),
        };
        mgr.finish_login("linear", &pending, resp).await.unwrap();
        assert!(mgr.pending_paste_login("linear").await.is_none());
        let token = crate::auth::load_namespaced_token_at(&mgr.auth_path, "linear").unwrap();
        assert_eq!(token.access_token, "atok");
        assert_eq!(token.refresh_token, "rtok");
        assert_eq!(token.token_endpoint, "https://example.test/token");
        assert_eq!(token.provider, "linear");
        assert_eq!(mgr.statuses().await[0].1, ServerStatus::Disconnected);
    }

    #[tokio::test]
    async fn pending_logins_returns_sorted_names_and_includes_orphans() {
        // `linear` is in cfg; `zed-mcp` + `ghost` are not — surfacing all
        // three is the point (orphans get separately filed by cli_show_status).
        let cfg: McpConfig = serde_json::from_str(linear_custom_cfg()).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        assert!(mgr.pending_logins().is_empty());
        seed_pending(&mgr, "zed-mcp", "s1");
        seed_pending(&mgr, "linear", "s2");
        seed_pending(&mgr, "ghost", "s3");
        let names = mgr.pending_logins();
        assert_eq!(names, vec!["ghost", "linear", "zed-mcp"]);
    }

    fn dead_oauth_cfg() -> &'static str {
        // 127.0.0.1:1 dials hermetically (no reachable MCP server) so
        // tests can prove the connect() reached the dial — i.e. the
        // oauth branch didn't short-circuit at NeedsAuth — without any
        // network round-trip.
        r#"{
            "mcpServers": {
                "linear": {
                    "type": "http",
                    "url": "http://127.0.0.1:1/mcp",
                    "oauth": {
                        "provider": "linear",
                        "authorize_url": "https://linear.app/oauth/authorize",
                        "token_url": "https://api.linear.app/oauth/token",
                        "client_id": "linear-client",
                        "scopes": ["read"]
                    }
                }
            }
        }"#
    }

    fn seed_token_with_refresh(
        mgr: &McpRuntimeManager,
        name: &str,
        expires_at: u64,
        refresh_token: &str,
    ) {
        let store = TokenStore {
            access_token: format!("atok-{name}"),
            refresh_token: refresh_token.to_string(),
            expires_at,
            token_endpoint: "http://127.0.0.1:1/token".to_string(),
            provider: "linear".to_string(),
        };
        save_namespaced_token_at(&mgr.auth_path, name, &store).unwrap();
    }

    fn seed_token(mgr: &McpRuntimeManager, name: &str, expires_at: u64) {
        seed_token_with_refresh(mgr, name, expires_at, "rtok");
    }

    #[tokio::test]
    async fn connect_oauth_with_valid_cached_token_attempts_dial_not_needs_auth() {
        // Valid token cached → connect() must NOT bounce at NeedsAuth.
        // Dial reaches the dead address and fails at handshake — that
        // failure surface is the proof the bearer was injected.
        let cfg: McpConfig = serde_json::from_str(dead_oauth_cfg()).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        seed_token(&mgr, "linear", u64::MAX);
        let err = mgr.connect("linear").await.unwrap_err().to_string();
        assert!(err.contains("handshake"), "expected 'handshake' in {err}");
        match &mgr.statuses().await[0].1 {
            ServerStatus::Failed(_) => {}
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn connect_oauth_expired_no_refresh_token_bounces_to_needs_auth() {
        // Expired token + empty refresh_token → no refresh attempt;
        // bounce directly to NeedsAuth. Proves the empty-refresh guard
        // short-circuits before the refresh POST.
        let cfg: McpConfig = serde_json::from_str(dead_oauth_cfg()).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        seed_token_with_refresh(&mgr, "linear", 0, "");
        let err = mgr.connect("linear").await.unwrap_err().to_string();
        assert!(err.contains("needs oauth login"), "got: {err}");
        assert_eq!(mgr.statuses().await[0].1, ServerStatus::NeedsAuth);
    }

    #[tokio::test]
    async fn connect_oauth_expired_with_refresh_token_failed_refresh_bounces_to_needs_auth() {
        // Expired token + non-empty refresh_token → refresh attempted;
        // refresh fails (custom-provider not yet supported in this slice,
        // or dead token_endpoint) → NeedsAuth bounce with refresh-failed
        // message. Proves the refresh path runs and that any failure
        // surfaces as user-actionable NeedsAuth.
        let cfg: McpConfig = serde_json::from_str(dead_oauth_cfg()).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        seed_token(&mgr, "linear", 0);
        let err = mgr.connect("linear").await.unwrap_err().to_string();
        assert!(err.contains("oauth refresh failed"), "got: {err}");
        assert_eq!(mgr.statuses().await[0].1, ServerStatus::NeedsAuth);
    }

    #[tokio::test]
    async fn finish_login_tolerates_provider_omitting_refresh_token() {
        let cfg: McpConfig = serde_json::from_str(linear_custom_cfg()).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        let pending = seed_pending(&mgr, "linear", "s");
        let resp = TokenExchangeResponse {
            access_token: "atok".to_string(),
            refresh_token: None,
            expires_in: None,
        };
        mgr.finish_login("linear", &pending, resp).await.unwrap();
        let token = crate::auth::load_namespaced_token_at(&mgr.auth_path, "linear").unwrap();
        assert_eq!(token.access_token, "atok");
        assert!(token.refresh_token.is_empty());
        // Long-lived sentinel: no `expires_in` from the provider must NOT
        // cause an immediate-expiry / refresh-loop / NeedsAuth bounce on
        // first use (Mira Tick 46 catch).
        assert_eq!(token.expires_at, u64::MAX);
    }
}

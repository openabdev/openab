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
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use rmcp::model::{
    ClientInfo, CreateElicitationRequestParams, CreateElicitationResult, CreateMessageRequestParams,
    CreateMessageResult, ErrorData, ListRootsResult, LoggingLevel, LoggingMessageNotificationParam,
    Root, RootsCapabilities, SamplingCapability, SetLevelRequestParams,
};
use rmcp::service::{NotificationContext, RequestContext, RoleClient, RunningService};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{ConfigureCommandExt, StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{ClientHandler, ServiceExt};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::RwLock;
use tokio::task::AbortHandle;

use super::breaker::{ServerBreaker, Verdict};
use super::config::{parse_logging_level, McpConfig, ServerConfig};
use super::flow::{canonical_resource, init_paste_authorize, parse_paste_callback};
use super::oauth::{builtin_client_id, resolve, ResolvedProvider};
use crate::auth::{
    auth_path, list_pending_logins_at, load_namespaced_token_at, load_pending_login, pending_key,
    remove_pending_login, save_namespaced_token_at, save_pending_login, PendingPasteLogin,
    TokenStore,
};

/// MCP client-side callback handler. Replaces the unit type `()` so individual
/// `ClientHandler` callbacks can be overridden (the named struct is the
/// keystone that unlocks `on_tool_list_changed` / `on_resource_updated` /
/// `on_prompt_list_changed` / elicitation-complete wiring later). Overrides
/// `get_info`, advertising the `roots` capability (without `listChanged`: the
/// root set is the agent's static working directory plus a fixed config
/// allow-list, so it never changes mid-session), and `list_roots`, returning
/// that set as `file://` URIs (spec rows 363-384); and `create_elicitation`,
/// returning `-32602` (invalid params) instead of the SDK default's silent
/// decline, because we advertise no `elicitation` capability (spec row 439).
/// Per-server cache of the most recent successful `tools/list` page, keyed by
/// configured server name. Deliberately a sibling `Arc` on the manager rather
/// than a field on `ServerHandle`: the handler holds a clone of this same `Arc`
/// and evicts its own entry on `notifications/tools/list_changed`, so parking it
/// on `ServerHandle` (which transitively owns the handler via `RunningService`)
/// would close an `Arc` cycle and leak. `StdMutex` is fine — the lock only
/// guards `HashMap` ops, never held across `.await` (row 503).
type ToolsCache = Arc<StdMutex<HashMap<String, Vec<rmcp::model::Tool>>>>;

#[derive(Clone, Debug, Default)]
pub struct OpenabClientHandler {
    /// Configured server name this connection belongs to. One handler
    /// instance per connection, so the handler can evict its own cache entry
    /// without the notification context (which carries no string server id).
    /// Empty only for the `Default` handler used by `get_info()`-only tests.
    server_name: String,
    /// Clone of the manager's per-server tools cache (`ToolsCache`). On
    /// `tools/list_changed` this handler drops its `server_name` entry so the
    /// next `fetch_tools` re-fetches (row 503).
    tools_cache: ToolsCache,
    /// The `roots` this client advertises and returns from `list_roots`.
    /// Shared (`Arc`) read-only across every connection's handler; computed
    /// once at manager construction (spec rows 363-384).
    roots: Arc<Vec<Root>>,
    /// Provider used to serve server-initiated `sampling/createMessage`
    /// (spec §390). `None` when no LLM credentials are configured — in which
    /// case `get_info` does not advertise the `sampling` capability, so a
    /// well-behaved server never sends a sampling request. Shared (`Arc` via
    /// `SharedLlmProvider`) so the handler stays `Clone` per connection.
    provider: Option<crate::llm::SharedLlmProvider>,
}

impl OpenabClientHandler {
    fn new(
        server_name: String,
        tools_cache: ToolsCache,
        roots: Arc<Vec<Root>>,
        provider: Option<crate::llm::SharedLlmProvider>,
    ) -> Self {
        Self {
            server_name,
            tools_cache,
            roots,
            provider,
        }
    }

    /// Evict this connection's cached `tools/list` page. Called from
    /// `on_tool_list_changed`; factored out so it can be unit-tested without
    /// fabricating a `NotificationContext` (row 503).
    fn invalidate_tools_cache(&self) {
        if let Ok(mut cache) = self.tools_cache.lock() {
            cache.remove(&self.server_name);
        }
        tracing::debug!(
            target: "mcp.cache",
            server = %self.server_name,
            "tools/list_changed: invalidated tools cache"
        );
    }
}

impl ClientHandler for OpenabClientHandler {
    fn get_info(&self) -> ClientInfo {
        // Advertise the `roots` capability so servers know they may call
        // `roots/list`. No `listChanged`: the root set is fixed for the
        // session (working dir + config allow-list), so we never emit
        // `notifications/roots/list_changed` (spec rows 363-384). Everything
        // else stays at the SDK default, byte-identical to the prior posture.
        let mut info = ClientInfo::default();
        info.capabilities.roots = Some(RootsCapabilities { list_changed: None });
        // Advertise `sampling` only when an LLM provider is configured — the
        // capability is a promise we can actually serve `create_message`. No
        // `tools` sub-capability: text-only baseline (`sampling.tools` is a
        // known gap), so tool-enabled requests are rejected (spec §390).
        if self.provider.is_some() {
            info.capabilities.sampling = Some(SamplingCapability::default());
        }
        info
    }

    fn list_roots(
        &self,
        _context: RequestContext<RoleClient>,
    ) -> impl std::future::Future<Output = Result<ListRootsResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListRootsResult::new((*self.roots).clone())))
    }

    fn create_elicitation(
        &self,
        _request: CreateElicitationRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> impl std::future::Future<Output = Result<CreateElicitationResult, ErrorData>> + Send + '_
    {
        // We advertise no `elicitation` capability, so a server MUST NOT send
        // this request. Reject explicitly with -32602 instead of inheriting the
        // SDK default's silent decline, so the violation is observable (row 439).
        std::future::ready(Err(ErrorData::invalid_params(
            "elicitation capability not declared",
            None,
        )))
    }

    fn create_message(
        &self,
        request: CreateMessageRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> impl std::future::Future<Output = Result<CreateMessageResult, ErrorData>> + Send + '_ {
        async move {
            // We advertise `sampling` only when a provider is configured, so a
            // request without one is a protocol violation (row 439 analog).
            let Some(provider) = self.provider.clone() else {
                return Err(ErrorData::invalid_params(
                    "sampling capability not declared",
                    None,
                ));
            };
            // Non-interactive consent gate (locked SamplingApproval env-var).
            super::sampling::approval_gate(super::sampling::SamplingApproval::from_env())?;
            // Text-only baseline: reject tool-enabled requests — we declare no
            // `sampling.tools` sub-capability (spec row 387a analog).
            if request.tools.as_ref().is_some_and(|t| !t.is_empty()) {
                return Err(ErrorData::invalid_params(
                    "tool-enabled sampling not supported (no sampling.tools capability)",
                    None,
                ));
            }
            let messages = super::sampling::convert_messages(&request.messages)?;
            let system = request.system_prompt.unwrap_or_default();
            let events = provider
                .chat(&system, &messages, &[])
                .await
                .map_err(|e| ErrorData::internal_error(super::concise_error_message(&e), None))?;
            let text = super::sampling::collect_text(events)?;
            Ok(super::sampling::build_result(text, provider.model()))
        }
    }

    fn on_logging_message(
        &self,
        params: LoggingMessageNotificationParam,
        context: NotificationContext<RoleClient>,
    ) -> impl std::future::Future<Output = ()> + Send + '_ {
        let server = context
            .peer
            .peer_info()
            .map(|i| i.server_info.name.clone())
            .unwrap_or_else(|| "<unknown>".to_string());
        let logger = params.logger.clone().unwrap_or_default();

        // Never log `params.data` contents — a compromised server could smuggle
        // secrets through its log payloads (row 590 is aspirational). Record only
        // the JSON shape and, for strings, the byte length.
        let data_kind = match &params.data {
            serde_json::Value::Null => "null",
            serde_json::Value::Bool(_) => "bool",
            serde_json::Value::Number(_) => "number",
            serde_json::Value::String(_) => "string",
            serde_json::Value::Array(_) => "array",
            serde_json::Value::Object(_) => "object",
        };
        let data_bytes = match &params.data {
            serde_json::Value::String(s) => s.len(),
            _ => 0,
        };

        match params.level {
            LoggingLevel::Debug => tracing::debug!(
                target: "mcp.server_log",
                server = %server, logger = %logger, level = "debug",
                data_kind, data_bytes, "mcp server log message"
            ),
            LoggingLevel::Info | LoggingLevel::Notice => tracing::info!(
                target: "mcp.server_log",
                server = %server, logger = %logger, level = "info",
                data_kind, data_bytes, "mcp server log message"
            ),
            LoggingLevel::Warning => tracing::warn!(
                target: "mcp.server_log",
                server = %server, logger = %logger, level = "warning",
                data_kind, data_bytes, "mcp server log message"
            ),
            LoggingLevel::Error
            | LoggingLevel::Critical
            | LoggingLevel::Alert
            | LoggingLevel::Emergency => tracing::error!(
                target: "mcp.server_log",
                server = %server, logger = %logger, level = ?params.level,
                data_kind, data_bytes, "mcp server log message"
            ),
        }

        std::future::ready(())
    }

    fn on_tool_list_changed(
        &self,
        _context: NotificationContext<RoleClient>,
    ) -> impl std::future::Future<Output = ()> + Send + '_ {
        // The server announced its tool set changed: drop the cached page so
        // the next `fetch_tools` re-fetches (row 503). We identify the server
        // by this handler's own `server_name` — one handler per connection —
        // because the notification context carries no string server id.
        self.invalidate_tools_cache();
        std::future::ready(())
    }
}

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
    pub client: Option<Arc<RunningService<RoleClient, OpenabClientHandler>>>,
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

/// Public return of `start_device_login` (RFC 8628 §3.2 user-facing
/// bundle). `verification_uri_complete` is the §3.3.1 extension that
/// pre-fills the user_code into the QR/link target; clients should
/// prefer it when present and fall back to the
/// `verification_uri` + `user_code` pair.
#[derive(Debug, Clone)]
pub struct DeviceLoginStart {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
}

/// Immutable, lock-free view of a configured server for catalogue
/// advertising in the system prompt (PR #959 chaodu F1, discovery slice).
/// Lives outside the `RwLock<HashMap>` so `format_system_prompt_appendix`
/// can build the prompt synchronously at `Agent::new_with_provider` time
/// without coordinating with the async runtime.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub name: String,
    pub transport: &'static str,
    pub requires_oauth: bool,
}

/// Owns one `ServerHandle` per configured server, behind an async `RwLock`
/// so the foreground LLM path and the background eviction task can share it.
#[derive(Debug, Clone)]
pub struct McpRuntimeManager {
    handles: Arc<RwLock<HashMap<String, ServerHandle>>>,
    /// `auth.json` location used for `mcp-pending:<server>` persistence.
    /// Injectable so tests can point at a tempdir instead of `$HOME`,
    /// avoiding cross-module HOME-env races (ADR §6.4).
    auth_path: PathBuf,
    /// Abort handle of the most-recent device-poll task per server. A
    /// fresh `start_device_login` aborts the prior poller so a retry
    /// after a transient failure doesn't leave two loops racing to
    /// finalize the same server. `std::sync::Mutex` is fine: the lock
    /// is only held for `HashMap` ops, never across `.await`.
    device_login_tasks: Arc<StdMutex<HashMap<String, AbortHandle>>>,
    /// Abort handle of the per-server periodic liveness-ping loop (rows
    /// 273-279). Installed when `connect()` succeeds for a server whose
    /// config opted in via `ping_interval_secs`; aborted on `disconnect`
    /// and on a fresh `connect` (so a reconnect replaces, not duplicates,
    /// the loop). Same `StdMutex` discipline as `device_login_tasks`: the
    /// lock only guards `HashMap` ops, never held across `.await`.
    ping_tasks: Arc<StdMutex<HashMap<String, AbortHandle>>>,
    /// Per-server single-flight gate for refresh-grant requests. The
    /// outer `StdMutex` guards the map (held only for `entry().or_insert`
    /// ops, never across `.await`); the inner `tokio::Mutex` is held
    /// across the network round-trip + disk write so concurrent waiters
    /// observe the winner's rotated token instead of replaying a stale
    /// refresh_token (which providers like Google would cascade-revoke).
    refresh_locks: Arc<StdMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// Per-server circuit breaker (ADR §5.9). Counts consecutive
    /// transport-level failures; once tripped, short-circuits `connect`
    /// and tool-call dispatch until the cooldown elapses and a
    /// half-open probe succeeds.
    breaker: Arc<ServerBreaker>,
    /// Sorted-by-name snapshot of static server identity (name + transport +
    /// oauth-required flag). Frozen at `from_config` — never mutated, so it
    /// is safe to read without locking. Used by the system-prompt catalogue
    /// (PR #959 F1 discovery slice).
    catalog: Arc<[CatalogEntry]>,
    /// Per-server `tools/list` cache (see `ToolsCache`). Populated by
    /// `fetch_tools`, evicted by `OpenabClientHandler::on_tool_list_changed`
    /// (which holds a clone of this exact `Arc`). Spares the hot tool-call
    /// path a `tools/list` round-trip for the task-support guard (row 503).
    tools_cache: ToolsCache,
    /// `roots` advertised to every server (spec rows 363-384). Computed once
    /// from the working directory + `McpConfig.roots` allow-list; cloned into
    /// each connection's `OpenabClientHandler`. Static for the session, so no
    /// `notifications/roots/list_changed` is ever sent.
    roots: Arc<Vec<Root>>,
    /// Shared LLM provider used to serve server-initiated sampling (spec §390).
    /// Resolved once at construction from `OPENAB_AGENT_PROVIDER` + credentials;
    /// `None` when none are available, in which case connections do not
    /// advertise the `sampling` capability. Cloned into each handler.
    provider: Option<crate::llm::SharedLlmProvider>,
}

impl McpRuntimeManager {
    pub fn from_config(cfg: McpConfig) -> Self {
        Self::from_config_with_auth_path(cfg, auth_path())
    }

    pub fn from_config_with_auth_path(cfg: McpConfig, auth_path: PathBuf) -> Self {
        let mut catalog: Vec<CatalogEntry> = cfg
            .servers
            .iter()
            .map(|(name, config)| CatalogEntry {
                name: name.clone(),
                transport: config.transport_label(),
                requires_oauth: config.requires_oauth(),
            })
            .collect();
        catalog.sort_by(|a, b| a.name.cmp(&b.name));
        let roots = Arc::new(compute_roots(std::env::current_dir().ok(), &cfg.roots));
        let provider = crate::llm::default_provider();
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
            device_login_tasks: Arc::new(StdMutex::new(HashMap::new())),
            ping_tasks: Arc::new(StdMutex::new(HashMap::new())),
            refresh_locks: Arc::new(StdMutex::new(HashMap::new())),
            breaker: Arc::new(ServerBreaker::new()),
            catalog: catalog.into(),
            tools_cache: Arc::new(StdMutex::new(HashMap::new())),
            roots,
            provider,
        }
    }

    /// Cached `tools/list` page for `server`, if a prior `fetch_tools`
    /// populated it and no `tools/list_changed` invalidated it since. Clones
    /// out so callers hold no lock across the result (row 503).
    pub(crate) fn cached_tools(&self, server: &str) -> Option<Vec<rmcp::model::Tool>> {
        self.tools_cache.lock().ok()?.get(server).cloned()
    }

    /// Store the freshly-fetched `tools/list` page for `server` (row 503).
    pub(crate) fn store_tools(&self, server: &str, tools: &[rmcp::model::Tool]) {
        if let Ok(mut cache) = self.tools_cache.lock() {
            cache.insert(server.to_string(), tools.to_vec());
        }
    }

    /// Lock-free, synchronous access to the configured-server catalogue.
    /// See `CatalogEntry` for the rationale.
    pub fn catalog(&self) -> &[CatalogEntry] {
        &self.catalog
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
    pub async fn arc_peer(
        &self,
        name: &str,
    ) -> Result<Arc<RunningService<RoleClient, OpenabClientHandler>>> {
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

    /// Per-request timeout configured for `name` (ADR §5.6). Read out from
    /// under a short read lock so call sites can pass it to rmcp's
    /// `send_request_with_option` without holding a runtime lock across the
    /// request. Falls back to the schema default for an unknown server.
    pub async fn request_timeout(&self, name: &str) -> Duration {
        let guard = self.handles.read().await;
        guard
            .get(name)
            .map(|h| h.config.request_timeout())
            .unwrap_or_else(|| Duration::from_secs(60))
    }

    /// Tear down a live server connection (ADR §5.4 shutdown ladder).
    ///
    /// Takes the `Arc<RunningService>` out under a short write lock and flips
    /// the status to `Disconnected`, then drops the lock before signalling the
    /// cancellation token so no runtime lock is held across teardown. Cancelling
    /// the token breaks rmcp's serve loop (`QuitReason::Cancelled`), which calls
    /// `transport.close()` → `TokioChildProcess::graceful_shutdown`: stdin is
    /// closed, the child is given a fixed grace window, then SIGKILLed.
    ///
    /// Best-effort: `cancellation_token().cancel()` is the only teardown path
    /// reachable through the shared `Arc` (rmcp's `close()`/`cancel()` need
    /// owned/`&mut` access). It is fire-and-forget — we cannot `await` the
    /// child reap here — and rmcp emits no SIGTERM rung, so this is the partial
    /// ladder the SDK exposes today.
    #[allow(dead_code)] // shutdown entry point wired by the eviction/quit path next slice
    pub async fn disconnect(&self, name: &str) -> Result<()> {
        let client = {
            let mut handles = self.handles.write().await;
            let handle = handles
                .get_mut(name)
                .ok_or_else(|| anyhow!("no mcp server named {name:?}"))?;
            handle.status = ServerStatus::Disconnected;
            handle.client.take()
        };
        self.abort_ping(name);
        if let Some(client) = client {
            client.cancellation_token().cancel();
        }
        Ok(())
    }

    /// Abort any in-flight periodic-ping loop for `name` (rows 273-279).
    /// Lock held only for the `HashMap::remove`, never across `.await`.
    fn abort_ping(&self, name: &str) {
        if let Some(handle) = self.ping_tasks.lock().unwrap().remove(name) {
            handle.abort();
        }
    }

    /// Spawn a per-server liveness-ping loop (MCP §5 ping / rows 273-279).
    /// Fires `ping` every `interval`, bounding each request by `timeout` via
    /// rmcp's cancellable-request path (auto-emits `notifications/cancelled`
    /// on expiry). A timeout or transport error is a liveness fault: it warns
    /// on `mcp.ping` and feeds the breaker `record_failure` so a hung server
    /// trips the circuit even when no foreground tool call is in flight. A
    /// healthy reply is genuine transport-level success → `record_success`.
    /// The loop holds an `Arc<RunningService>`; on `disconnect` the
    /// `AbortHandle` (stored in `ping_tasks`) is aborted and the Arc drops.
    fn spawn_ping_loop(
        &self,
        name: String,
        client: Arc<RunningService<RoleClient, OpenabClientHandler>>,
        interval: Duration,
        timeout: Duration,
    ) {
        let manager = self.clone();
        let key = name.clone();
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // First tick fires immediately; skip it so the first ping waits a
            // full interval after connect rather than racing the handshake.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let mut options = rmcp::service::PeerRequestOptions::no_options();
                options.timeout = Some(timeout);
                let request =
                    rmcp::model::ClientRequest::PingRequest(rmcp::model::PingRequest::default());
                let outcome = match client.send_request_with_option(request, options).await {
                    Ok(handle) => handle.await_response().await,
                    Err(e) => Err(e),
                };
                match outcome {
                    Ok(rmcp::model::ServerResult::EmptyResult(_)) => {
                        manager.breaker.record_success(&name);
                    }
                    Ok(other) => {
                        tracing::warn!(
                            target: "mcp.ping",
                            server = %name,
                            "unexpected ping response: {other:?}"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "mcp.ping",
                            server = %name,
                            "ping failed: {}",
                            super::redact_secrets(&format!("{e}"))
                        );
                        manager.breaker.record_failure(&name);
                    }
                }
            }
        });
        if let Some(prev) = self.ping_tasks.lock().unwrap().insert(key, task.abort_handle()) {
            prev.abort();
        }
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
        let (provider, client_id, redirect_uri, resource) =
            self.resolve_paste_client(name).await?;
        let started =
            init_paste_authorize(&provider, &client_id, &redirect_uri, resource.as_deref())?;
        let pending = PendingPasteLogin {
            verifier: started.code_verifier,
            state: started.state.clone(),
            token_url: provider.token_url().to_string(),
            provider_name: provider_name_of(&provider),
            resource,
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
    /// no entry or the file is unreadable. `mcp status` surfaces in-flight
    /// logins via `list_pending_logins_at`; this accessor is the single-
    /// entry counterpart for callers that need the full snapshot.
    #[allow(dead_code)] // accessor for future per-entry status detail
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
        let pending =
            load_pending_login(&self.auth_path, &pending_key(name)).with_context(|| {
                format!("no pending login for {name:?}; run `mcp login {name}` first")
            })?;
        let code = parse_paste_callback(redirect_url, &pending.state)?;
        let (_provider, client_id, redirect_uri, _resource) =
            self.resolve_paste_client(name).await?;
        // Use the snapshotted `resource` (RFC 8707) so a config edit between
        // init and finish can't redirect the token's audience binding —
        // matching the same snapshot rule as `token_url`/`provider_name`.
        let resp = post_token_exchange(
            &pending.token_url,
            &client_id,
            &redirect_uri,
            &code,
            &pending.verifier,
            pending.resource.as_deref(),
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
        let store = build_token_store(
            resp,
            pending.token_url.clone(),
            pending.provider_name.clone(),
            None,
        );
        save_namespaced_token_at(&self.auth_path, name, &store)?;
        remove_pending_login(&self.auth_path, &pending_key(name))?;
        let mut handles = self.handles.write().await;
        if let Some(handle) = handles.get_mut(name) {
            handle.status = ServerStatus::Disconnected;
        }
        Ok(())
    }

    /// Begin a device-code OAuth login (ADR §6.4 + RFC 8628) for an HTTP
    /// server whose `oauth:` block declares a `device_authorization_endpoint`
    /// (§6.3). Built-in providers don't yet ship device endpoints — that
    /// requires a `ProviderSpec` schema extension (out of scope this slice).
    ///
    /// 1. POST RFC 8628 §3.1 device authorization → user_code +
    ///    verification_uri + interval + expires_in
    /// 2. Spawn a detached `tokio::task` that drives the §3.4 polling loop,
    ///    persists the `TokenStore` on success, and writes server status
    ///    (`Disconnected` on success so the next `connect()` picks up the
    ///    cached token; `NeedsAuth` on terminal failure)
    /// 3. Return the user-facing bundle (the polling task is fire-and-
    ///    forget — observed via `mcp status`)
    ///
    /// Choosing `Disconnected` over the ADR's "transitions to Connected"
    /// keeps the polling task out of the MCP handshake path. The next
    /// `connect()` reads the cached token via the oauth-aware `DialPlan`
    /// branch and reaches `Connected` through the normal lifecycle.
    pub async fn start_device_login(&self, name: &str) -> Result<DeviceLoginStart> {
        let (device_endpoint, client_id, token_url, scopes, provider_name, resource) =
            self.resolve_device_client(name).await?;
        let auth = post_device_authorization(
            &device_endpoint,
            &client_id,
            &scopes.join(" "),
            resource.as_deref(),
        )
        .await?;
        {
            let mut handles = self.handles.write().await;
            if let Some(handle) = handles.get_mut(name) {
                handle.status = ServerStatus::Connecting;
            }
        }
        let manager = self.clone();
        let name_owned = name.to_string();
        let device_code = auth.device_code.clone();
        let initial_interval = auth.interval;
        let expires_in = auth.expires_in;
        let token_url_owned = token_url;
        let client_id_owned = client_id;
        let provider_name_owned = provider_name;
        let resource_owned = resource;
        let task_name = name.to_string();
        let handle = tokio::spawn(async move {
            manager
                .run_device_poll_loop(
                    &name_owned,
                    &token_url_owned,
                    &client_id_owned,
                    &device_code,
                    &provider_name_owned,
                    resource_owned.as_deref(),
                    initial_interval,
                    expires_in,
                )
                .await;
        });
        let prior = {
            let mut tasks = self
                .device_login_tasks
                .lock()
                .expect("device_login_tasks mutex poisoned");
            tasks.insert(task_name, handle.abort_handle())
        };
        if let Some(prior) = prior {
            prior.abort();
        }
        Ok(DeviceLoginStart {
            user_code: auth.user_code,
            verification_uri: auth.verification_uri,
            verification_uri_complete: auth.verification_uri_complete,
            expires_in: auth.expires_in,
        })
    }

    /// Resolve `(device_endpoint, client_id, token_url, scopes, provider_name)`
    /// for `name`. Rejects non-Http / non-oauth / built-in / missing-endpoint
    /// configurations with explicit errors so the user sees what to fix in
    /// `mcp.json`.
    async fn resolve_device_client(
        &self,
        name: &str,
    ) -> Result<(String, String, String, Vec<String>, String, Option<String>)> {
        let (oauth_cfg, server_url) = {
            let guard = self.handles.read().await;
            let handle = guard
                .get(name)
                .ok_or_else(|| anyhow!("no mcp server named {name:?}"))?;
            match handle.config.resolved(name)? {
                ServerConfig::Http {
                    url,
                    oauth: Some(oauth),
                    ..
                } => (oauth, url),
                ServerConfig::Http { oauth: None, .. } => {
                    return Err(anyhow!("mcp server {name:?} has no oauth block"));
                }
                ServerConfig::Stdio { .. } => {
                    return Err(anyhow!("mcp server {name:?} is stdio, not http+oauth"));
                }
            }
        };
        let provider = resolve(&oauth_cfg)?;
        let ResolvedProvider::Custom {
            provider_name,
            token_url,
            client_id: Some(client_id),
            device_authorization_endpoint: Some(device_endpoint),
            scopes,
            ..
        } = provider
        else {
            return Err(anyhow!(
                "mcp server {name:?} device-flow requires a Custom provider with \
                 both `oauth.device_authorization_endpoint` and `oauth.client_id` \
                 set in mcp.json"
            ));
        };
        // RFC 8707 resource indicator — device flow only fires for Custom
        // providers (the `let-else` above), so the server URL is always the
        // audience here (see `resolve_paste_client` for the gating rationale).
        let resource = Some(canonical_resource(&server_url)?);
        Ok((
            device_endpoint,
            client_id,
            token_url,
            scopes,
            provider_name,
            resource,
        ))
    }

    /// RFC 8628 §3.4 polling loop. Runs detached in `tokio::spawn`; the
    /// only observable side-effect is `auth.json` (on Success) + the
    /// `ServerHandle.status` transition. Errors are logged via `tracing`
    /// and surface to the user via `mcp status` (Failed/NeedsAuth).
    #[allow(clippy::too_many_arguments)]
    async fn run_device_poll_loop(
        &self,
        name: &str,
        token_url: &str,
        client_id: &str,
        device_code: &str,
        provider_name: &str,
        resource: Option<&str>,
        initial_interval: u64,
        expires_in_secs: u64,
    ) {
        // One client across the whole loop — `reqwest::Client` is an
        // `Arc`-backed connection pool, so reusing it keeps TLS / TCP
        // handshakes amortized across the dozens-to-hundreds of polls a
        // 30-minute device-flow window can produce.
        let client = match reqwest::Client::builder().build() {
            Ok(c) => c,
            Err(e) => {
                self.mark_device_login_failed(name, anyhow!("build reqwest client: {e}"))
                    .await;
                return;
            }
        };
        let deadline = now_secs().saturating_add(expires_in_secs);
        let mut interval = initial_interval;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            if now_secs() >= deadline {
                self.mark_device_login_failed(
                    name,
                    anyhow!("device-flow expired before user authorized"),
                )
                .await;
                return;
            }
            let outcome = match post_device_token_poll(
                &client, token_url, client_id, device_code, resource,
            )
            .await
            {
                    Ok(o) => o,
                    Err(e) => {
                        self.mark_device_login_failed(name, e).await;
                        return;
                    }
                };
            match outcome {
                DevicePollOutcome::Success(resp) => {
                    self.finalize_device_login(name, provider_name, token_url, resp)
                        .await;
                    return;
                }
                DevicePollOutcome::AuthorizationPending => continue,
                DevicePollOutcome::SlowDown => {
                    // RFC 8628 §3.5: SlowDown means add 5s to the interval.
                    interval = interval.saturating_add(5);
                }
                DevicePollOutcome::AccessDenied => {
                    self.mark_device_login_failed(name, anyhow!("device-flow denied by user"))
                        .await;
                    return;
                }
                DevicePollOutcome::ExpiredToken => {
                    self.mark_device_login_failed(name, anyhow!("device_code expired"))
                        .await;
                    return;
                }
            }
        }
    }

    /// Pure-persistence tail of `run_device_poll_loop` on RFC 8628 §3.5
    /// Success.
    async fn finalize_device_login(
        &self,
        name: &str,
        provider_name: &str,
        token_url: &str,
        resp: TokenExchangeResponse,
    ) {
        let store = build_token_store(resp, token_url.to_string(), provider_name.to_string(), None);
        if let Err(e) = save_namespaced_token_at(&self.auth_path, name, &store) {
            self.mark_device_login_failed(name, e).await;
            return;
        }
        let mut handles = self.handles.write().await;
        if let Some(handle) = handles.get_mut(name) {
            handle.status = ServerStatus::Disconnected;
        }
    }

    async fn mark_device_login_failed(&self, name: &str, err: anyhow::Error) {
        tracing::warn!(server = %name, error = %err, "device-flow polling failed");
        let mut handles = self.handles.write().await;
        if let Some(handle) = handles.get_mut(name) {
            handle.status = ServerStatus::NeedsAuth;
        }
    }

    /// Resolve a paste-back OAuth client `(provider, client_id, redirect_uri)`
    /// from the server's config. Shared by `start_paste_login` and
    /// `complete_login` so a config drift between init and finish surfaces
    /// the same error from both entry points.
    async fn resolve_paste_client(
        &self,
        name: &str,
    ) -> Result<(ResolvedProvider, String, String, Option<String>)> {
        let (oauth_cfg, server_url) = {
            let guard = self.handles.read().await;
            let handle = guard
                .get(name)
                .ok_or_else(|| anyhow!("no mcp server named {name:?}"))?;
            match handle.config.resolved(name)? {
                ServerConfig::Http {
                    url,
                    oauth: Some(oauth),
                    ..
                } => (oauth, url),
                ServerConfig::Http { oauth: None, .. } => {
                    return Err(anyhow!("mcp server {name:?} has no oauth block"));
                }
                ServerConfig::Stdio { .. } => {
                    return Err(anyhow!("mcp server {name:?} is stdio, not http+oauth"));
                }
            }
        };
        let provider = resolve(&oauth_cfg)?;
        // RFC 8707 resource indicator. Gated to custom providers: a built-in's
        // authorize/token endpoints point at the vendor AS (claude.ai), not the
        // MCP server's own URL, and there's no evidence the built-in AS honors
        // `resource` — sending it risks an `invalid_target` rejection that would
        // break the shipping built-in login. Custom providers are
        // self-hosted-resource-server style where the server URL *is* the
        // audience. Revisit once PRM/discovery (Rows 153-168) lands.
        let resource = match &provider {
            ResolvedProvider::Builtin { .. } => None,
            ResolvedProvider::Custom { .. } => Some(canonical_resource(&server_url)?),
        };
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
            ResolvedProvider::Custom {
                client_id: Some(client_id),
                redirect_uri: Some(redirect_uri),
                ..
            } => (client_id.clone(), redirect_uri.clone()),
            ResolvedProvider::Custom {
                client_id: None, ..
            } => {
                return Err(anyhow!(
                    "mcp server {name:?} custom paste-back requires `oauth.client_id` in mcp.json"
                ));
            }
            ResolvedProvider::Custom {
                redirect_uri: None, ..
            } => {
                return Err(anyhow!(
                    "mcp server {name:?} custom paste-back requires `oauth.redirect_uri` in mcp.json \
                     (must match the redirect URL pre-registered with the provider)"
                ));
            }
        };
        Ok((provider, client_id, redirect_uri, resource))
    }

    /// RFC 6749 §6 refresh-grant — exchange a cached `refresh_token` for a
    /// new `access_token`. Resolves `client_id` from current config (so a
    /// rotated builtin catalog entry is picked up automatically). Per
    /// ADR §6.6 rotation contract: if the provider omits a new
    /// `refresh_token` in the response, the previous one is preserved
    /// (Google-style rotation); the agent fsyncs `auth.json` before
    /// returning so deployment-side mtime watchers can sync the rotated
    /// token to peer replicas.
    ///
    /// Per-server single-flight: concurrent `connect()` callers serialize
    /// on `refresh_locks[name]`. After acquiring the lock, the function
    /// re-reads the on-disk token; if a prior waiter already refreshed,
    /// the cached store is returned without a second POST. This prevents
    /// replayed-refresh cascade-revokes on providers like Google.
    async fn try_refresh_oauth_token(&self, name: &str, store: &TokenStore) -> Result<TokenStore> {
        if store.refresh_token.is_empty() {
            return Err(anyhow!("no refresh_token cached for {name:?}"));
        }
        let lock = {
            let mut locks = self
                .refresh_locks
                .lock()
                .expect("refresh_locks mutex poisoned");
            locks
                .entry(name.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;
        if let Ok(cached) = load_namespaced_token_at(&self.auth_path, name) {
            if !cached.is_expired() {
                return Ok(cached);
            }
        }
        let (_provider, client_id, _redirect_uri, resource) =
            self.resolve_paste_client(name).await?;
        let resp = post_token_refresh(
            &store.token_endpoint,
            &client_id,
            &store.refresh_token,
            resource.as_deref(),
        )
        .await?;
        let new_store = build_token_store(
            resp,
            store.token_endpoint.clone(),
            store.provider.clone(),
            Some(store.refresh_token.clone()),
        );
        save_namespaced_token_at(&self.auth_path, name, &new_store)?;
        Ok(new_store)
    }

    /// Lazy-connect the named server (ADR §5.7). Idempotent if already
    /// `Connected` with a live client. HTTP servers with an `oauth:` block
    /// are routed through `mcp login` first — `connect` marks them
    /// `NeedsAuth` and returns an error pointing the caller at the login
    /// subcommand rather than attempting an unauthenticated dial.
    pub async fn connect(&self, name: &str) -> Result<()> {
        // Connect-time `logging/setLevel` value (MCP §16 / row 584), captured
        // from config before `resolved` is consumed by the dial-plan match so
        // we can issue `set_level` once the handshake succeeds below. Assigned
        // on every path that reaches the dial; earlier paths return.
        let connect_log_level: Option<LoggingLevel>;
        let plan = {
            let mut guard = self.handles.write().await;
            let handle = guard
                .get_mut(name)
                .ok_or_else(|| anyhow!("no mcp server named {name:?}"))?;
            // Check the breaker before the connected fast path. Tool-call
            // transport failures can open the breaker while the client handle
            // remains installed; those calls must still be short-circuited
            // until the cooldown/probe cycle succeeds.
            if let Verdict::Reject { retry_in_secs } = self.breaker.check(name) {
                return Err(anyhow!(
                    "mcp server {name:?} circuit-breaker open — retry in {retry_in_secs}s"
                ));
            }
            if matches!(handle.status, ServerStatus::Connected) && handle.client.is_some() {
                return Ok(());
            }
            let resolved = handle.config.resolved(name)?;
            connect_log_level = resolved.log_level().and_then(parse_logging_level);
            let plan = match resolved {
                ServerConfig::Stdio {
                    command, args, env, ..
                } => DialPlan::Dial(Dial::Stdio { command, args, env }),
                ServerConfig::Http {
                    url,
                    oauth: Some(_),
                    ..
                } => match load_namespaced_token_at(&self.auth_path, name) {
                    Ok(store) if !store.is_expired() => DialPlan::Dial(Dial::Http {
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
                            // A concurrent connect() may have refreshed +
                            // dialed successfully while we were awaiting
                            // our (failed) refresh. Don't clobber the
                            // winner's Connected status with NeedsAuth.
                            if !matches!(h.status, ServerStatus::Connected) {
                                h.status = ServerStatus::NeedsAuth;
                            }
                        }
                        return Err(anyhow!(
                            "mcp server {name:?} oauth refresh failed: {e:#} — run `mcp login {name}`"
                        ));
                    }
                }
            }
        };

        let dial_result = dial
            .run(
                name,
                self.tools_cache.clone(),
                self.roots.clone(),
                self.provider.clone(),
            )
            .await;

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
                // Apply the operator-pinned MCP log level (row 584). Optional
                // capability — a failure must not abort an otherwise healthy
                // connection, so we warn and continue.
                if let Some(level) = connect_log_level {
                    if let Err(e) = client.set_level(SetLevelRequestParams::new(level)).await {
                        tracing::warn!(
                            target: "mcp.server_log",
                            server = %name,
                            "logging/setLevel failed: {e:#}"
                        );
                    }
                }
                handle.status = ServerStatus::Connected;
                let client = Arc::new(client);
                handle.client = Some(client.clone());
                self.breaker.record_success(name);
                // Opt-in periodic liveness ping (rows 273-279). A reconnect
                // installs a fresh loop; `spawn_ping_loop` aborts any prior
                // one for this server so loops never accumulate.
                if let Some((interval, timeout)) = handle.config.ping_config() {
                    self.spawn_ping_loop(name.to_string(), client, interval, timeout);
                }
                Ok(())
            }
            Err(e) => {
                // Full (redacted) chain to tracing for operators; concise
                // (redacted) message to the caller-facing status + returned
                // error (row 37b: brevity for the LLM, detail in the logs).
                tracing::warn!(
                    server = %name,
                    "mcp connect failed: {}",
                    super::redact_secrets(&format!("{e:#}"))
                );
                handle.status = ServerStatus::Failed(super::concise_error_message(&e));
                self.breaker.record_failure(name);
                Err(anyhow!(super::concise_error_message(&e)))
            }
        }
    }

    /// Record a tool-call outcome on the breaker. Called from
    /// `meta_tool::call_tool` after `peer.call_tool().await` returns.
    /// Wire-level `Ok` resets the counter regardless of `CallToolResult.is_error`
    /// (the `isError` bit is protocol-normal payload, not a transport fault).
    /// Wire-level `Err` is a transport-level failure and increments the
    /// counter — matching the single-counter / transport-only model from
    /// the #966 design decisions.
    pub fn record_tool_call_outcome(&self, name: &str, ok: bool) {
        if ok {
            self.breaker.record_success(name);
        } else {
            self.breaker.record_failure(name);
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

/// Lift a token-endpoint response into the on-disk `TokenStore` shape.
/// `expires_in: None` → `u64::MAX` sentinel (treated as never-expires by
/// `TokenStore::is_expired`); a `now + 0` would mark the token already
/// expired and bounce the user back through login on the next connect().
/// `fallback_refresh` preserves the previous refresh token on rotation
/// when the provider omits one (ADR §6.6 Google-style); fresh logins
/// pass `None` so an omitted refresh token records as empty.
fn build_token_store(
    resp: TokenExchangeResponse,
    token_endpoint: String,
    provider: String,
    fallback_refresh: Option<String>,
) -> TokenStore {
    let expires_at = match resp.expires_in {
        Some(secs) => now_secs().saturating_add(secs),
        None => u64::MAX,
    };
    let refresh_token = resp.refresh_token.or(fallback_refresh).unwrap_or_default();
    TokenStore {
        access_token: resp.access_token,
        refresh_token,
        expires_at,
        token_endpoint,
        provider,
    }
}

/// Shared POST helper for both `post_token_exchange` (RFC 6749 §4.1.3)
/// and `post_token_refresh` (RFC 6749 §6). Public client — no
/// `client_secret`. Errors fold body text into the message so transient
/// 4xx from the provider land in the user's terminal verbatim.
async fn post_token_form(
    token_url: &str,
    form: &[(&str, &str)],
    grant_label: &str,
) -> Result<TokenExchangeResponse> {
    let client = reqwest::Client::builder()
        .build()
        .context("build reqwest client")?;
    let resp = client
        .post(token_url)
        .form(form)
        .send()
        .await
        .with_context(|| format!("POST {token_url} ({grant_label})"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("token endpoint returned {status}: {body}"));
    }
    serde_json::from_str(&body).map_err(|e| anyhow!("invalid token response: {e}; body={body}"))
}

async fn post_token_exchange(
    token_url: &str,
    client_id: &str,
    redirect_uri: &str,
    code: &str,
    code_verifier: &str,
    resource: Option<&str>,
) -> Result<TokenExchangeResponse> {
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", code_verifier),
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
    ];
    if let Some(resource) = resource {
        form.push(("resource", resource));
    }
    post_token_form(token_url, &form, "token exchange").await
}

async fn post_token_refresh(
    token_url: &str,
    client_id: &str,
    refresh_token: &str,
    resource: Option<&str>,
) -> Result<TokenExchangeResponse> {
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    if let Some(resource) = resource {
        form.push(("resource", resource));
    }
    post_token_form(token_url, &form, "token refresh").await
}

/// RFC 8628 §3.2 device authorization response. `verification_uri_complete`
/// is the §3.3.1 extension (`verification_uri` + `user_code` is the always-
/// present fallback the agent relays to the user). `interval` defaults to
/// 5s per RFC 8628 §3.5 when omitted by the provider.
#[derive(Debug, serde::Deserialize)]
struct DeviceAuthResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default = "default_device_poll_interval")]
    interval: u64,
}

fn default_device_poll_interval() -> u64 {
    5
}

/// RFC 8628 §3.5 polling outcome. The four named "errors"
/// (`authorization_pending`, `slow_down`, `access_denied`, `expired_token`)
/// are flow-level states NOT real failures — they drive the polling loop.
/// Everything else folds into a fatal `Err` at the call site.
#[derive(Debug)]
enum DevicePollOutcome {
    Success(TokenExchangeResponse),
    AuthorizationPending,
    SlowDown,
    AccessDenied,
    ExpiredToken,
}

/// Pure response classifier — split from the HTTP path so the RFC 8628
/// §3.5 error-code mapping is unit-testable without a mock server. 2xx
/// parses as a token response; 4xx parses `{"error": "..."}` and maps the
/// four flow-state codes to enum variants; everything else (including
/// non-JSON / unknown error codes) folds into `Err`.
fn classify_device_poll(status: reqwest::StatusCode, body: &str) -> Result<DevicePollOutcome> {
    if status.is_success() {
        return serde_json::from_str(body)
            .map(DevicePollOutcome::Success)
            .map_err(|e| anyhow!("invalid token response: {e}; body={body}"));
    }
    #[derive(serde::Deserialize)]
    struct ErrBody {
        error: String,
    }
    let err_code = serde_json::from_str::<ErrBody>(body).ok().map(|e| e.error);
    match err_code.as_deref() {
        Some("authorization_pending") => Ok(DevicePollOutcome::AuthorizationPending),
        Some("slow_down") => Ok(DevicePollOutcome::SlowDown),
        Some("access_denied") => Ok(DevicePollOutcome::AccessDenied),
        Some("expired_token") => Ok(DevicePollOutcome::ExpiredToken),
        _ => Err(anyhow!("token endpoint returned {status}: {body}")),
    }
}

/// POST to the RFC 8628 §3.1 device authorization endpoint. Public client
/// — no `client_secret`. Returns the `{device_code, user_code, ...}`
/// bundle the runtime relays to the user and polls against the token
/// endpoint via `post_device_token_poll`.
async fn post_device_authorization(
    device_endpoint: &str,
    client_id: &str,
    scopes: &str,
    resource: Option<&str>,
) -> Result<DeviceAuthResponse> {
    let client = reqwest::Client::builder()
        .build()
        .context("build reqwest client")?;
    let mut form = vec![("client_id", client_id), ("scope", scopes)];
    if let Some(resource) = resource {
        form.push(("resource", resource));
    }
    let resp = client
        .post(device_endpoint)
        .form(&form)
        .send()
        .await
        .with_context(|| format!("POST {device_endpoint} (device authorization)"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!(
            "device authorization endpoint returned {status}: {body}"
        ));
    }
    serde_json::from_str(&body)
        .map_err(|e| anyhow!("invalid device authorization response: {e}; body={body}"))
}

/// POST one polling tick to the token endpoint per RFC 8628 §3.4. Caller
/// owns the polling loop (interval, expires_in deadline, SlowDown back-
/// off). Returns a `DevicePollOutcome` so the loop can distinguish the
/// four RFC 8628 §3.5 flow states from real errors.
async fn post_device_token_poll(
    client: &reqwest::Client,
    token_url: &str,
    client_id: &str,
    device_code: &str,
    resource: Option<&str>,
) -> Result<DevicePollOutcome> {
    let mut form = vec![
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ("device_code", device_code),
        ("client_id", client_id),
    ];
    if let Some(resource) = resource {
        form.push(("resource", resource));
    }
    let resp = client
        .post(token_url)
        .form(&form)
        .send()
        .await
        .with_context(|| format!("POST {token_url} (device token poll)"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    classify_device_poll(status, &body)
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
    async fn run(
        self,
        name: &str,
        tools_cache: ToolsCache,
        roots: Arc<Vec<Root>>,
        provider: Option<crate::llm::SharedLlmProvider>,
    ) -> Result<RunningService<RoleClient, OpenabClientHandler>> {
        match self {
            Dial::Stdio { command, args, env } => {
                let cmd = Command::new(&command).configure(|c| {
                    c.env_clear();
                    c.envs(stdio_child_env(&env));
                    c.args(&args);
                });
                // rmcp's `TokioChildProcess::new` inherits the child's stderr,
                // so `npx`/server startup errors vanish into container stderr.
                // Pipe it and tee each line into `tracing` tagged by server
                // (ADR §5.4 observability; spec Row 79). The reader task ends
                // on child exit (stderr EOF → `next_line` → `Ok(None)`).
                let (transport, stderr) = TokioChildProcess::builder(cmd)
                    .stderr(Stdio::piped())
                    .spawn()
                    .with_context(|| format!("spawn mcp child process {command:?}"))?;
                if let Some(stderr) = stderr {
                    let server = name.to_string();
                    tokio::spawn(async move {
                        let mut lines = BufReader::new(stderr).lines();
                        while let Ok(Some(line)) = lines.next_line().await {
                            tracing::warn!(server = %server, "mcp stderr: {line}");
                        }
                    });
                }
                OpenabClientHandler::new(name.to_string(), tools_cache, roots, provider)
                    .serve(transport)
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
                OpenabClientHandler::new(name.to_string(), tools_cache, roots, provider)
                    .serve(transport)
                    .await
                    .with_context(|| format!("mcp handshake with {url:?}"))
            }
        }
    }
}

/// Build the MCP `roots` advertised to servers: the agent's working directory
/// followed by any `McpConfig.roots` allow-list entries (spec rows 363-384).
/// Each candidate is `canonicalize`d — which resolves `..` and symlinks to a
/// real absolute path, neutralizing path traversal (#372) — and kept only if
/// it resolves to an existing directory. Duplicates (after canonicalization)
/// are dropped so a config entry equal to the cwd isn't advertised twice.
/// Roots are returned as `file://` URIs named by their final path component.
fn compute_roots(cwd: Option<PathBuf>, extra: &[String]) -> Vec<Root> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let candidates = cwd
        .into_iter()
        .chain(extra.iter().map(|s| PathBuf::from(s.as_str())));
    for raw in candidates {
        let Ok(canonical) = raw.canonicalize() else {
            continue;
        };
        if !canonical.is_dir() {
            continue;
        }
        // An absolute path renders as `/a/b`, so `file://` + `/a/b` yields the
        // correct three-slash `file:///a/b` form.
        let uri = format!("file://{}", canonical.display());
        if !seen.insert(uri.clone()) {
            continue;
        }
        let name = canonical
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| uri.clone());
        out.push(Root::new(uri).with_name(name));
    }
    out
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
        assert!(mgr.catalog().is_empty());
    }

    #[test]
    fn client_handler_advertises_only_roots_capability() {
        // Pins the capability posture: we declare `roots` (and serve it from
        // `list_roots`), but still abstain from sampling (`create_message`)
        // and elicitation (`create_elicitation`, -32602). `roots` carries no
        // `listChanged` — the root set is static for the session. If a future
        // change wires sampling/elicitation/tasks it MUST flip the matching
        // capability, and this test will fail, forcing a deliberate re-audit
        // (spec rows 363-384/439, §390).
        let caps = OpenabClientHandler::default().get_info().capabilities;
        let roots = caps.roots.expect("must advertise roots");
        assert_eq!(
            roots.list_changed, None,
            "roots must not advertise listChanged (static root set)"
        );
        assert!(
            caps.sampling.is_none(),
            "no provider configured (Default handler) → must not advertise sampling"
        );
        assert!(caps.elicitation.is_none(), "must not advertise elicitation");
        assert!(caps.tasks.is_none(), "must not advertise tasks");
    }

    #[derive(Debug)]
    struct StubProvider;
    impl crate::llm::LlmProvider for StubProvider {
        fn model(&self) -> &str {
            "stub-model"
        }
        fn chat<'a>(
            &'a self,
            _system: &'a str,
            _messages: &'a [crate::llm::Message],
            _tools: &'a [crate::llm::ToolDef],
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<crate::llm::LlmEvent>>> + Send + 'a>,
        > {
            Box::pin(async { Ok(vec![crate::llm::LlmEvent::Text("ok".into())]) })
        }
    }

    #[test]
    fn handler_with_provider_advertises_text_only_sampling() {
        // When a provider IS configured, the handler advertises `sampling`
        // (text-only — no `tools` sub-capability) alongside `roots`. Flipping
        // this on with the provider is what makes `create_message` reachable
        // (spec §390); the no-provider case stays asserted above.
        let provider = crate::llm::SharedLlmProvider(Arc::new(StubProvider));
        let handler = OpenabClientHandler::new(
            "srv".to_string(),
            Arc::new(StdMutex::new(HashMap::new())),
            Arc::new(Vec::new()),
            Some(provider),
        );
        let caps = handler.get_info().capabilities;
        let sampling = caps.sampling.expect("provider present → advertise sampling");
        assert!(
            sampling.tools.is_none(),
            "text-only baseline must not advertise sampling.tools"
        );
        assert!(caps.roots.is_some(), "still advertises roots");
    }

    #[test]
    fn on_tool_list_changed_evicts_only_its_own_server() {
        // Two handlers sharing one cache (as connections do via the manager's
        // sibling `Arc`): each evicts only its own `server_name` entry, leaving
        // the others warm (row 503).
        let cache: ToolsCache = Arc::new(StdMutex::new(HashMap::new()));
        cache
            .lock()
            .unwrap()
            .insert("alpha".to_string(), Vec::new());
        cache.lock().unwrap().insert("beta".to_string(), Vec::new());

        let alpha = OpenabClientHandler::new(
            "alpha".to_string(),
            cache.clone(),
            Arc::new(Vec::new()),
            None,
        );
        alpha.invalidate_tools_cache();

        let guard = cache.lock().unwrap();
        assert!(!guard.contains_key("alpha"), "alpha entry must be evicted");
        assert!(guard.contains_key("beta"), "beta entry must survive");
    }

    #[test]
    fn compute_roots_canonicalizes_dedups_and_drops_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let sub = cwd.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let file = cwd.join("a-file");
        std::fs::write(&file, b"x").unwrap();

        let extra = vec![
            sub.to_string_lossy().into_owned(),
            // `cwd/sub/..` canonicalizes back to `cwd` — a duplicate of the
            // working-directory root, must be dropped.
            sub.join("..").to_string_lossy().into_owned(),
            // A regular file: not a directory, dropped.
            file.to_string_lossy().into_owned(),
            // Non-existent path: fails to canonicalize, dropped.
            cwd.join("nope").to_string_lossy().into_owned(),
        ];
        let roots = compute_roots(Some(cwd.clone()), &extra);

        let uris: Vec<&str> = roots.iter().map(|r| r.uri.as_str()).collect();
        assert_eq!(
            uris,
            vec![
                format!("file://{}", cwd.display()),
                format!("file://{}", sub.display()),
            ],
            "only cwd + sub survive, in order, deduped"
        );
        assert!(roots.iter().all(|r| r.name.is_some()), "roots are named");
    }

    #[test]
    fn handler_carries_advertised_roots() {
        // `list_roots` returns `ListRootsResult::new((*self.roots).clone())`,
        // but its `RequestContext` can't be fabricated without a live `Peer`
        // (same harness gap as the capability test), so assert on the field
        // the override closes over — that is what gets returned verbatim.
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let roots = Arc::new(compute_roots(Some(cwd.clone()), &[]));
        let handler = OpenabClientHandler::new(
            "srv".to_string(),
            Arc::new(StdMutex::new(HashMap::new())),
            roots.clone(),
            None,
        );
        assert_eq!(*handler.roots, *roots);
        assert_eq!(handler.roots.len(), 1);
        assert_eq!(handler.roots[0].uri, format!("file://{}", cwd.display()));
    }

    #[test]
    fn catalog_is_sorted_and_flags_oauth() {
        let json = r#"{
            "mcpServers": {
                "linear": {
                    "type": "http",
                    "url": "https://mcp.linear.app/mcp",
                    "oauth": { "provider": "linear", "scopes": ["read"] }
                },
                "fs": { "type": "stdio", "command": "mcp-server-filesystem" },
                "weather": { "type": "http", "url": "https://example/mcp" }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        let cat = mgr.catalog();
        let names: Vec<&str> = cat.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["fs", "linear", "weather"]);
        let by_name: std::collections::HashMap<&str, &CatalogEntry> =
            cat.iter().map(|e| (e.name.as_str(), e)).collect();
        assert_eq!(by_name["fs"].transport, "stdio");
        assert!(!by_name["fs"].requires_oauth);
        assert_eq!(by_name["linear"].transport, "http");
        assert!(by_name["linear"].requires_oauth);
        assert_eq!(by_name["weather"].transport, "http");
        assert!(!by_name["weather"].requires_oauth);
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

    #[tokio::test]
    async fn breaker_opens_after_threshold_consecutive_connect_failures() {
        // 127.0.0.1:1 hermetic dead-port (same pattern as the test above).
        // After FAIL_THRESHOLD dial failures the breaker trips, and the
        // next connect() short-circuits with the cooldown hint instead of
        // attempting another dial.
        let json = r#"{
            "mcpServers": {
                "dead": { "type": "http", "url": "http://127.0.0.1:1/mcp" }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let mgr = McpRuntimeManager::from_config(cfg);
        for _ in 0..crate::mcp::breaker::FAIL_THRESHOLD {
            assert!(mgr.connect("dead").await.is_err());
        }
        let err = mgr.connect("dead").await.unwrap_err().to_string();
        assert!(
            err.contains("circuit-breaker open"),
            "expected breaker hint in {err}"
        );
        assert!(err.contains("retry in"), "expected retry hint in {err}");
    }

    #[tokio::test]
    async fn breaker_does_not_count_oauth_needs_auth_bounces() {
        // NeedsAuth is an auth-level state, not a transport-level failure;
        // the breaker must NOT trip after repeated NeedsAuth returns.
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
        for _ in 0..(crate::mcp::breaker::FAIL_THRESHOLD + 2) {
            let err = mgr.connect("linear").await.unwrap_err().to_string();
            assert!(
                err.contains("needs oauth login"),
                "expected NeedsAuth bounce, got {err}"
            );
        }
    }

    // start_paste_login + builtin_client_id race on the same OS env var —
    // `set_var` is unsound under concurrent reads, so serialize them.
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
    async fn start_paste_login_rejects_custom_without_redirect_uri() {
        let cfg: McpConfig = serde_json::from_str(linear_custom_cfg()).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        let err = start_login_err(&mgr, "linear").await;
        assert!(err.contains("oauth.redirect_uri"), "got: {err}");
        assert!(mgr.pending_paste_login("linear").await.is_none());
    }

    #[tokio::test]
    async fn start_paste_login_rejects_custom_without_client_id() {
        let json = r#"{
            "mcpServers": {
                "linear": {
                    "type": "http",
                    "url": "https://mcp.linear.app/mcp",
                    "oauth": {
                        "provider": "linear",
                        "authorize_url": "https://linear.app/oauth/authorize",
                        "token_url": "https://api.linear.app/oauth/token",
                        "redirect_uri": "https://example.com/cb"
                    }
                }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        let err = start_login_err(&mgr, "linear").await;
        assert!(err.contains("oauth.client_id"), "got: {err}");
        assert!(mgr.pending_paste_login("linear").await.is_none());
    }

    #[tokio::test]
    async fn start_paste_login_custom_with_client_id_and_redirect_uri_succeeds() {
        let json = r#"{
            "mcpServers": {
                "linear": {
                    "type": "http",
                    "url": "https://mcp.linear.app/mcp",
                    "oauth": {
                        "provider": "linear",
                        "authorize_url": "https://linear.app/oauth/authorize",
                        "token_url": "https://api.linear.app/oauth/token",
                        "client_id": "linear-client",
                        "redirect_uri": "https://example.com/cb",
                        "scopes": ["read"]
                    }
                }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        let start = mgr.start_paste_login("linear").await.unwrap();
        assert!(start.authorize_url.contains("client_id=linear-client"));
        assert!(start.authorize_url.contains("redirect_uri=https"));
        let pending = mgr.pending_paste_login("linear").await.unwrap();
        assert_eq!(pending.state, start.state);
        assert_eq!(pending.provider_name, "linear");
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
            resource: Some("https://mcp.linear.app/sse".to_string()),
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
        // first use.
        assert_eq!(token.expires_at, u64::MAX);
    }

    #[test]
    fn classify_device_poll_decodes_success_into_token() {
        let body = r#"{"access_token": "atk", "refresh_token": "rtk", "expires_in": 3600}"#;
        let outcome = classify_device_poll(reqwest::StatusCode::OK, body).unwrap();
        let DevicePollOutcome::Success(token) = outcome else {
            panic!("expected Success");
        };
        assert_eq!(token.access_token, "atk");
        assert_eq!(token.refresh_token.as_deref(), Some("rtk"));
        assert_eq!(token.expires_in, Some(3600));
    }

    #[test]
    fn classify_device_poll_maps_rfc8628_flow_states() {
        let cases = [
            ("authorization_pending", "AuthorizationPending"),
            ("slow_down", "SlowDown"),
            ("access_denied", "AccessDenied"),
            ("expired_token", "ExpiredToken"),
        ];
        for (code, want) in cases {
            let body = format!(r#"{{"error": "{code}"}}"#);
            let outcome = classify_device_poll(reqwest::StatusCode::BAD_REQUEST, &body).unwrap();
            let got = match outcome {
                DevicePollOutcome::AuthorizationPending => "AuthorizationPending",
                DevicePollOutcome::SlowDown => "SlowDown",
                DevicePollOutcome::AccessDenied => "AccessDenied",
                DevicePollOutcome::ExpiredToken => "ExpiredToken",
                DevicePollOutcome::Success(_) => "Success",
            };
            assert_eq!(got, want, "code={code}");
        }
    }

    #[test]
    fn classify_device_poll_folds_unknown_error_into_err() {
        let body = r#"{"error": "invalid_grant"}"#;
        let err = classify_device_poll(reqwest::StatusCode::BAD_REQUEST, body)
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid_grant"), "got: {err}");
    }

    #[test]
    fn classify_device_poll_folds_non_json_5xx_into_err() {
        let err = classify_device_poll(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "<html>")
            .unwrap_err()
            .to_string();
        assert!(err.contains("500"), "got: {err}");
    }

    #[test]
    fn device_auth_response_defaults_interval_to_rfc8628_value() {
        let body = r#"{
            "device_code": "dc",
            "user_code": "AAAA-BBBB",
            "verification_uri": "https://example.com/device",
            "expires_in": 1800
        }"#;
        let resp: DeviceAuthResponse = serde_json::from_str(body).unwrap();
        assert_eq!(resp.interval, 5);
        assert!(resp.verification_uri_complete.is_none());
    }

    fn linear_device_cfg() -> &'static str {
        // 127.0.0.1:1 dials hermetically so tests can prove
        // start_device_login() reached the device-authorization POST —
        // i.e. config validation passed — without a network round-trip.
        r#"{
            "mcpServers": {
                "linear": {
                    "type": "http",
                    "url": "https://mcp.linear.app/mcp",
                    "oauth": {
                        "provider": "linear",
                        "authorize_url": "https://linear.app/oauth/authorize",
                        "token_url": "https://api.linear.app/oauth/token",
                        "device_authorization_endpoint": "http://127.0.0.1:1/device",
                        "client_id": "linear-client",
                        "scopes": ["read"]
                    }
                }
            }
        }"#
    }

    async fn start_device_err(mgr: &McpRuntimeManager, name: &str) -> String {
        mgr.start_device_login(name).await.unwrap_err().to_string()
    }

    #[tokio::test]
    async fn start_device_login_rejects_unknown_server() {
        let cfg: McpConfig = serde_json::from_str(linear_device_cfg()).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        let err = start_device_err(&mgr, "ghost").await;
        assert!(err.contains("ghost"), "got: {err}");
    }

    #[tokio::test]
    async fn start_device_login_rejects_stdio_server() {
        let json = r#"{
            "mcpServers": {
                "fs": {
                    "type": "stdio",
                    "command": "/bin/true"
                }
            }
        }"#;
        let cfg: McpConfig = serde_json::from_str(json).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        let err = start_device_err(&mgr, "fs").await;
        assert!(err.contains("stdio"), "got: {err}");
    }

    #[tokio::test]
    async fn start_device_login_rejects_custom_without_device_endpoint() {
        // linear_custom_cfg omits `device_authorization_endpoint` — the
        // paste-back fixture from earlier slices doubles as the negative
        // case here.
        let cfg: McpConfig = serde_json::from_str(linear_custom_cfg()).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        let err = start_device_err(&mgr, "linear").await;
        assert!(err.contains("device_authorization_endpoint"), "got: {err}");
    }

    #[tokio::test]
    async fn start_device_login_with_device_endpoint_reaches_http_post() {
        // Config validation passes (Custom + device_endpoint + client_id all
        // present) so the failure must come from the POST itself — proves
        // the gate didn't short-circuit before dial.
        let cfg: McpConfig = serde_json::from_str(linear_device_cfg()).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        let err = start_device_err(&mgr, "linear").await;
        assert!(
            !err.contains("device_authorization_endpoint"),
            "config validation should have passed; got: {err}"
        );
    }

    #[tokio::test]
    async fn try_refresh_short_circuits_when_disk_has_fresh_token() {
        // Single-flight contract: if another waiter has already refreshed
        // (fresh token on disk), `try_refresh_oauth_token` must return the
        // cached store without POSTing to the dead `token_endpoint`. The
        // input `store` is intentionally stale (zero `expires_at`) and
        // points at 127.0.0.1:1 — any POST attempt would surface a connect
        // error, so a successful return proves the re-check ran.
        let cfg: McpConfig = serde_json::from_str(dead_oauth_cfg()).unwrap();
        let (mgr, _dir) = mgr_with_tempdir(cfg);
        seed_token(&mgr, "linear", u64::MAX);
        let stale = TokenStore {
            access_token: "stale".to_string(),
            refresh_token: "rtok".to_string(),
            expires_at: 0,
            token_endpoint: "http://127.0.0.1:1/token".to_string(),
            provider: "linear".to_string(),
        };
        let fresh = mgr.try_refresh_oauth_token("linear", &stale).await.unwrap();
        assert_eq!(fresh.access_token, "atok-linear");
        assert!(!fresh.is_expired());
    }
}

//! The runtime's single listening surface: attach WebSockets plus the remote
//! admin plane.
//!
//! Three ADR properties are enforced here rather than documented and hoped for:
//!
//! 1. **Attach is attach-only.** `GET /pty/{session}` verifies a bearer attach
//!    token and nothing else. It holds an [`AttachVerifier`], which has no
//!    issuance API at all, so "no minting path on the attach surface" is a
//!    property of the type this handler was given, not of a routing convention.
//! 2. **The admin plane is remote-only, and authentication is its whole
//!    boundary.** These HTTP routes are the *entire* admin plane: there is
//!    deliberately no UDS, no second loopback-only listener, and no in-container
//!    CLI, so there is no path on which being *inside* the container authorizes
//!    anything. What that does **not** mean is unreachability: a managed session
//!    shares this container's network namespace and can connect to this listener,
//!    and the adversary suite asserts exactly that — the request arrives and is
//!    answered `401`, because the credential lives on the operator's side and
//!    never enters the container. Minted tokens are returned in the HTTP response
//!    to the external control client and are never written to a PTY master.
//! 3. **`Origin` is not consulted.** Possession of a valid attach token is the
//!    sole authorization. The as-built `/acp` endpoint checks `Origin` only on
//!    its *keyless* loopback path; PTY has no keyless mode, so there is nothing
//!    to carry over, and `Origin` is attacker-controlled outside browsers.
//!
//! The bearer transport is the validated `/acp` scheme:
//! `Authorization: Bearer <token>` for non-browser clients and
//! `Sec-WebSocket-Protocol: openab.bearer.<token>` for browsers (which cannot
//! set the `Authorization` header on an upgrade), with the constant-time
//! comparison living in [`crate::token`] / [`crate::admin_auth`].

use crate::admin_auth::{AdminAuthFailure, AdminAuthenticator, MAX_ADMIN_CREDENTIAL_BYTES};
use crate::audit::{AuditEvent, AuditKind, AuditLogger};
use crate::close_code;
use crate::containment::SecretBytes;
use crate::session::{
    AttachedConnection, ControlEvent, InputError, OutboundItem, SessionManager, WindowSize,
};
use crate::token::AttachVerifier;
use crate::{Error, SessionName};
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Wire contract
// ---------------------------------------------------------------------------

/// Bearer subprotocol prefix, identical to the as-built `/acp` scheme.
pub const BEARER_SUBPROTOCOL_PREFIX: &str = "openab.bearer.";
/// The real subprotocol echoed on a successful upgrade, so a browser that
/// offered `openab.bearer.<token>, openab.pty.v1` completes its handshake.
pub const PTY_SUBPROTOCOL: &str = "openab.pty.v1";
/// Control-schema version carried on every text frame in both directions. A
/// frame without it, or with a different value, is rejected rather than guessed.
pub const CONTROL_SCHEMA_VERSION: u64 = 1;
/// Max PTY-bytes (binary) frame accepted from a client.
pub const MAX_INPUT_FRAME_BYTES: usize = 64 * 1024;
/// Max control (text) frame accepted from a client.
pub const MAX_CONTROL_FRAME_BYTES: usize = 4 * 1024;
/// Malformed control frames tolerated before the connection is dropped.
pub const MAX_MALFORMED_FRAMES: u32 = 3;
/// Upper bound on the presented attach token before it is hashed, so one
/// rejected upgrade cannot turn into unbounded work.
pub const MAX_ATTACH_TOKEN_BYTES: usize = 512;
/// Admin request body cap. The admin credential travels in a header, never a
/// body, so bodies only ever carry a small JSON object.
pub const MAX_ADMIN_BODY_BYTES: usize = 4 * 1024;

/// Per-IP WS-upgrade failure limit, then a short ban (ADR: "e.g. 5 failures/min
/// then a short ban").
pub const UPGRADE_FAILURE_LIMIT: u32 = 5;
pub const UPGRADE_FAILURE_WINDOW: Duration = Duration::from_secs(60);
pub const UPGRADE_FAILURE_BAN: Duration = Duration::from_secs(60);
/// Bound on tracked source addresses, so the limiter itself cannot be turned
/// into a memory-growth vector by rotating source IPs.
const MAX_TRACKED_SOURCES: usize = 4096;

/// RFC 6455 normal closure. A client-initiated `detach` is not an anomaly, so it
/// deliberately does **not** consume one of the ADR's application close codes.
const WS_NORMAL_CLOSURE: u16 = 1000;
/// RFC 6455 policy violation: malformed or oversized frames. Frame abuse is a
/// protocol-level fault, distinct from the lifecycle codes in
/// [`crate::close_code`], and mixing the two would make lifecycle reporting lie.
const WS_POLICY_VIOLATION: u16 = 1008;

/// A parsed, allowlisted client control frame. Anything not in this enum is
/// rejected — the parser has no fallthrough.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientControl {
    Resize(WindowSize),
    /// Application-level keepalive for clients behind proxies that eat WS pings.
    Ping,
    Detach,
}

/// Why a client frame was refused. Every variant increments the abuse metric.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameReject {
    Oversize,
    NotJson,
    /// Missing or unsupported `v`.
    Version,
    /// `type` absent or outside the allowlist.
    UnknownType,
    /// `resize` outside `1..=1000`, or non-integer.
    ResizeBounds,
}

/// Strict allowlist parser for client → server text frames.
///
/// Pure, so the whole abuse surface is unit-testable without a socket.
pub fn parse_client_control(text: &str) -> Result<ClientControl, FrameReject> {
    if text.len() > MAX_CONTROL_FRAME_BYTES {
        return Err(FrameReject::Oversize);
    }
    let value: Value = serde_json::from_str(text).map_err(|_| FrameReject::NotJson)?;
    let object = value.as_object().ok_or(FrameReject::NotJson)?;
    if object.get("v").and_then(Value::as_u64) != Some(CONTROL_SCHEMA_VERSION) {
        return Err(FrameReject::Version);
    }
    match object.get("type").and_then(Value::as_str) {
        Some("resize") => {
            let rows = object
                .get("rows")
                .and_then(Value::as_u64)
                .ok_or(FrameReject::ResizeBounds)?;
            let cols = object
                .get("cols")
                .and_then(Value::as_u64)
                .ok_or(FrameReject::ResizeBounds)?;
            let rows = u16::try_from(rows).map_err(|_| FrameReject::ResizeBounds)?;
            let cols = u16::try_from(cols).map_err(|_| FrameReject::ResizeBounds)?;
            WindowSize::validated(rows, cols)
                .map(ClientControl::Resize)
                .map_err(|_| FrameReject::ResizeBounds)
        }
        Some("ping") => Ok(ClientControl::Ping),
        Some("detach") => Ok(ClientControl::Detach),
        _ => Err(FrameReject::UnknownType),
    }
}

/// Render a server → client control frame: the session module's event plus the
/// schema version, and optional server-owned fields (the attach frame's stream
/// offset) that belong to the wire contract rather than to session state.
fn control_frame(event: &ControlEvent, extra: &[(&str, Value)]) -> String {
    let mut value = serde_json::to_value(event).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("v".to_string(), json!(CONTROL_SCHEMA_VERSION));
        for (key, extra_value) in extra {
            map.insert((*key).to_string(), extra_value.clone());
        }
    }
    value.to_string()
}

fn pong_frame() -> String {
    json!({ "v": CONTROL_SCHEMA_VERSION, "type": "pong" }).to_string()
}

// ---------------------------------------------------------------------------
// Bearer extraction (identical scheme to `/acp`)
// ---------------------------------------------------------------------------

/// Token from the `openab.bearer.<token>` entry of a `Sec-WebSocket-Protocol`
/// offer. Browsers cannot set `Authorization` on a WS upgrade, so this is their
/// only header-borne path; it keeps the token out of the URL.
pub fn subprotocol_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("sec-websocket-protocol")
        .and_then(|value| value.to_str().ok())
        .and_then(|list| {
            list.split(',')
                .map(str::trim)
                .find_map(|entry| entry.strip_prefix(BEARER_SUBPROTOCOL_PREFIX))
        })
}

/// Attach bearer token, header-borne only.
///
/// There is deliberately no `?token=` fallback: a query string leaks the
/// capability into history, referrers, and proxy access logs. The same removal
/// was made on `/acp`.
pub fn ws_bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .or_else(|| subprotocol_token(headers))
}

/// Admin credential from `Authorization: Bearer <credential>`.
///
/// Header only: never an argv flag, never an environment variable, never a query
/// parameter, never a body field — per the ADR's credential-handling MUSTs.
fn admin_credential(headers: &HeaderMap) -> Option<SecretBytes> {
    let raw = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))?;
    if raw.is_empty() || raw.len() > MAX_ADMIN_CREDENTIAL_BYTES {
        return None;
    }
    Some(SecretBytes::from_slice(raw.as_bytes()))
}

// ---------------------------------------------------------------------------
// Fail-closed listener guard
// ---------------------------------------------------------------------------

/// Whether the listen address binds loopback. An unparseable host is treated as
/// non-loopback (fail safe), matching `/acp`.
pub fn bind_is_loopback(listen_addr: &str) -> bool {
    let host = listen_addr
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(listen_addr)
        .trim_matches(|c| c == '[' || c == ']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Whether the runtime may bind this address, given its auth material and TLS
/// declaration. Same shape as `acp_auth_ok_for_bind`, with the ADR's extra
/// clause: off-loopback plain WS is served only when the deployment declares
/// that a trusted upstream terminates TLS.
pub fn listener_guard(
    listen_addr: &str,
    admin_credential_hash: &str,
    tls_terminated_upstream: bool,
) -> Result<(), String> {
    if bind_is_loopback(listen_addr) {
        return Ok(());
    }
    if admin_credential_hash.is_empty() {
        return Err(format!(
            "refusing to bind {listen_addr}: no admin credential hash configured; an \
             unauthenticated admin plane must never be exposed off-loopback"
        ));
    }
    if !tls_terminated_upstream {
        return Err(format!(
            "refusing to bind {listen_addr}: tls_terminated_upstream = false and this runtime \
             holds no TLS key (the same-UID MVP forbids one), so an off-loopback bind would \
             serve plain WS without a trusted terminator"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Abuse metrics and per-IP upgrade limiting
// ---------------------------------------------------------------------------

/// Counters for the prevention paths. Audit is detection; these are the
/// prevention side, and they are separate from session metrics on purpose.
#[derive(Debug, Default)]
pub struct AbuseMetrics {
    pub malformed_frames: AtomicU64,
    pub oversize_frames: AtomicU64,
    pub upgrade_failures: AtomicU64,
    pub upgrade_bans: AtomicU64,
    pub input_backpressure_disconnects: AtomicU64,
    pub admin_auth_failures: AtomicU64,
}

impl AbuseMetrics {
    fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> Value {
        json!({
            "malformed_frames": self.malformed_frames.load(Ordering::Relaxed),
            "oversize_frames": self.oversize_frames.load(Ordering::Relaxed),
            "upgrade_failures": self.upgrade_failures.load(Ordering::Relaxed),
            "upgrade_bans": self.upgrade_bans.load(Ordering::Relaxed),
            "input_backpressure_disconnects":
                self.input_backpressure_disconnects.load(Ordering::Relaxed),
            "admin_auth_failures": self.admin_auth_failures.load(Ordering::Relaxed),
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct FailureWindow {
    window_start: Instant,
    failures: u32,
    banned_until: Option<Instant>,
    last_seen: Instant,
}

/// Per-source-address WS-upgrade failure limiter.
///
/// **Source semantics, stated because it is easy to get wrong**: this counts the
/// *peer socket address*. `X-Forwarded-For` is deliberately not trusted — it is
/// client-settable, so honouring it would let an attacker rotate a header value
/// to reset the bucket, and would let them ban a victim's address. Behind a
/// trusted proxy the limiter therefore degrades to per-proxy, which bounds work
/// but not per-client fairness; a deployment wanting per-client limits should
/// enforce them at the proxy, which is the only component that knows the real
/// peer.
#[derive(Debug)]
pub struct UpgradeFailureLimiter {
    limit: u32,
    window: Duration,
    ban: Duration,
    entries: parking_lot::Mutex<HashMap<IpAddr, FailureWindow>>,
}

impl Default for UpgradeFailureLimiter {
    fn default() -> Self {
        Self::new(
            UPGRADE_FAILURE_LIMIT,
            UPGRADE_FAILURE_WINDOW,
            UPGRADE_FAILURE_BAN,
        )
    }
}

impl UpgradeFailureLimiter {
    pub fn new(limit: u32, window: Duration, ban: Duration) -> Self {
        Self {
            limit,
            window,
            ban,
            entries: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// `Err(retry_after)` while the source is banned.
    pub fn check(&self, source: IpAddr) -> Result<(), Duration> {
        self.check_at(source, Instant::now())
    }

    fn check_at(&self, source: IpAddr, now: Instant) -> Result<(), Duration> {
        // Copy the decision out and release the guard on this line: holding a
        // parking_lot guard into an `if let` body is the self-deadlock this
        // crate already hit once.
        //
        // `last_seen` is refreshed here, including while banned. Without that, a
        // source that keeps knocking looks *stale* to LRU eviction, so distinct-IP
        // churn could evict an active ban and hand the banned source a reset.
        let banned_until = {
            let mut entries = self.entries.lock();
            match entries.get_mut(&source) {
                Some(entry) => {
                    entry.last_seen = now;
                    entry.banned_until
                }
                None => None,
            }
        };
        match banned_until {
            Some(until) if until > now => Err(until.saturating_duration_since(now)),
            _ => Ok(()),
        }
    }

    /// Record a failed upgrade. Returns `true` when this failure started a ban.
    pub fn record_failure(&self, source: IpAddr) -> bool {
        self.record_failure_at(source, Instant::now())
    }

    fn record_failure_at(&self, source: IpAddr, now: Instant) -> bool {
        let mut entries = self.entries.lock();
        Self::prune(&mut entries, now, self.window, self.ban);
        if entries.len() >= MAX_TRACKED_SOURCES && !entries.contains_key(&source) {
            // Every tracked source is still live. Evict the least recently seen
            // rather than growing, and rather than dropping the new observation:
            // an unbounded map is the worse failure of the two. Actively banned
            // entries are not eviction candidates — dropping one would let churn
            // across distinct addresses clear somebody's ban.
            if let Some(oldest) = entries
                .iter()
                .filter(|(_, entry)| !entry.banned_until.is_some_and(|until| until > now))
                .min_by_key(|(_, entry)| entry.last_seen)
                .map(|(ip, _)| *ip)
            {
                entries.remove(&oldest);
            } else {
                // Pathological: every slot is under an active ban. Leave the map
                // as it is rather than growing it or clearing a live ban; this
                // source goes untracked until one expires.
                return false;
            }
        }
        let entry = entries.entry(source).or_insert(FailureWindow {
            window_start: now,
            failures: 0,
            banned_until: None,
            last_seen: now,
        });
        entry.last_seen = now;
        if now.duration_since(entry.window_start) >= self.window {
            entry.window_start = now;
            entry.failures = 0;
        }
        entry.failures = entry.failures.saturating_add(1);
        if entry.failures >= self.limit {
            entry.failures = 0;
            entry.window_start = now;
            entry.banned_until = Some(now + self.ban);
            return true;
        }
        false
    }

    /// A successful attach clears the bucket for that source.
    pub fn on_success(&self, source: IpAddr) {
        self.entries.lock().remove(&source);
    }

    fn prune(
        entries: &mut HashMap<IpAddr, FailureWindow>,
        now: Instant,
        window: Duration,
        ban: Duration,
    ) {
        let stale = window.max(ban);
        entries.retain(|_, entry| {
            entry.banned_until.is_some_and(|until| until > now)
                || now.duration_since(entry.last_seen) < stale
        });
    }
}

// ---------------------------------------------------------------------------
// Server state
// ---------------------------------------------------------------------------

/// Listener-shaped settings, separated from [`crate::config::PtyConfig`] so tests
/// can bind an ephemeral port without a projection on disk.
///
/// DEFERRED to Gate B: promoting `drain_grace` and `tick_interval` to projection
/// knobs. They are settable here because tests need them, and the withdrawn-knob
/// pass deliberately left them out of the operator surface: every knob is a
/// fail-closed validation rule plus a documented default, and no deployment has
/// asked to change either value.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub listen: String,
    pub tls_terminated_upstream: bool,
    /// Grace between the shutdown notice and teardown.
    pub drain_grace: Duration,
    /// TTL maintenance interval.
    pub tick_interval: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8090".to_string(),
            tls_terminated_upstream: true,
            drain_grace: Duration::from_secs(3),
            tick_interval: Duration::from_secs(1),
        }
    }
}

/// Everything the routes need.
///
/// Note what is **absent**: no [`crate::token::TokenStore`]. The attach surface
/// holds only an [`AttachVerifier`], which cannot mint; the admin routes reach
/// minting through [`SessionManager`], which is gated by the admin credential.
pub struct AppState {
    pub manager: Arc<SessionManager>,
    pub verifier: AttachVerifier,
    pub admin: AdminAuthenticator,
    pub audit: AuditLogger,
    pub config: ServerConfig,
    pub abuse: AbuseMetrics,
    pub upgrades: UpgradeFailureLimiter,
    /// Set once shutdown begins: new attaches and creates are refused so the
    /// drain cannot be extended indefinitely by fresh work.
    draining: AtomicBool,
}

impl AppState {
    pub fn new(
        manager: Arc<SessionManager>,
        verifier: AttachVerifier,
        admin: AdminAuthenticator,
        audit: AuditLogger,
        config: ServerConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            manager,
            verifier,
            admin,
            audit,
            config,
            abuse: AbuseMetrics::default(),
            upgrades: UpgradeFailureLimiter::default(),
            draining: AtomicBool::new(false),
        })
    }

    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::Acquire)
    }

    fn begin_drain(&self) {
        self.draining.store(true, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// DEFERRED to Gate B (publishing an image/chart/docs): `/healthz` and `/metrics`
/// endpoints. Neither has a consumer while this crate is internal and unpublished
/// — there is no chart to write a probe into and no scrape target — and liveness
/// and counters are already observable through `GET /admin/sessions`. Adding a
/// second unauthenticated surface on the listener a managed session can reach is
/// not free, so it waits until something actually consumes it.
pub fn router(state: Arc<AppState>) -> Router {
    let admin = Router::new()
        .route("/admin/sessions", get(admin_list).post(admin_create))
        .route("/admin/sessions/{session}", delete(admin_kill))
        .route("/admin/sessions/{session}/renew", post(admin_renew))
        .route("/admin/sessions/{session}/restart", post(admin_restart))
        .layer(DefaultBodyLimit::max(MAX_ADMIN_BODY_BYTES));
    Router::new()
        .route("/pty/{session}", get(attach))
        .merge(admin)
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Attach
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct AttachQuery {
    /// Monotonic byte cursor for incremental reconnect.
    pub since: Option<u64>,
    pub rows: Option<u16>,
    pub cols: Option<u16>,
}

async fn attach(
    State(state): State<Arc<AppState>>,
    Path(session): Path<String>,
    Query(query): Query<AttachQuery>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let source = peer.ip();
    if let Err(retry_after) = state.upgrades.check(source) {
        AbuseMetrics::bump(&state.abuse.upgrade_bans);
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", retry_after.as_secs().max(1).to_string())],
        )
            .into_response();
    }
    if state.is_draining() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime is draining for shutdown",
        )
            .into_response();
    }

    // Everything below fails with an indistinguishable 401 on purpose: an
    // unauthenticated caller must not be able to enumerate session names.
    let rejected = |state: &Arc<AppState>| {
        AbuseMetrics::bump(&state.abuse.upgrade_failures);
        if state.upgrades.record_failure(source) {
            AbuseMetrics::bump(&state.abuse.upgrade_bans);
            tracing::warn!(%source, "banning source after repeated attach-upgrade failures");
        }
        StatusCode::UNAUTHORIZED.into_response()
    };

    let Some(token) = ws_bearer_token(&headers) else {
        return rejected(&state);
    };
    if token.is_empty() || token.len() > MAX_ATTACH_TOKEN_BYTES {
        return rejected(&state);
    }
    let Ok(name) = SessionName::parse(&session) else {
        return rejected(&state);
    };
    // The generation is read from the live session and re-checked inside
    // `attach`. A token minted for a superseded generation cannot pass: `renew`
    // and `kill` delete the stored hash before exposing the new generation.
    let Some(live) = state.manager.get(&name) else {
        return rejected(&state);
    };
    let generation = live.generation();
    let mut presented = SecretBytes::from_slice(token.as_bytes());
    let peer_label = peer.to_string();
    if state
        .verifier
        .verify(&name, generation, &mut presented, Some(&peer_label))
        .is_err()
    {
        return rejected(&state);
    }
    state.upgrades.on_success(source);

    let size = match WindowSize::validated(query.rows.unwrap_or(24), query.cols.unwrap_or(80)) {
        Ok(size) => size,
        Err(_) => return (StatusCode::BAD_REQUEST, "rows/cols out of range").into_response(),
    };

    let since = query.since;
    upgrade
        .protocols([PTY_SUBPROTOCOL])
        // Hard protocol-level backstop at twice the accepted size. The in-handler
        // check below is what increments the abuse metric and reports a policy
        // violation; this bound is what stops a client from making the protocol
        // layer buffer a multi-megabyte message before that check can run
        // (tungstenite defaults to 64 MiB per message).
        .max_message_size(2 * MAX_INPUT_FRAME_BYTES)
        .max_frame_size(2 * MAX_INPUT_FRAME_BYTES)
        .on_upgrade(move |socket| async move {
            serve_attached(state, socket, name, generation, size, since, peer_label).await;
        })
}

/// Map an attach failure onto a close code, so a client is told *why* instead of
/// seeing a bare disconnect.
fn attach_failure_close_code(error: &Error) -> u16 {
    match error {
        // The session died between the pre-upgrade generation read and the CAS.
        Error::SessionDead(_) | Error::NotFound(_) => close_code::SESSION_ENDED,
        // A renew landed in the same window: the token is for a stale generation.
        Error::Unauthorized => close_code::RENEWED,
        Error::CapacityExceeded { .. } => close_code::CAPACITY,
        // An internal fault is not capacity. Reporting CAPACITY here told the
        // client the exact wrong thing — that the runtime is full and it should
        // create another session — turning one runtime fault into a create loop.
        Error::Io(_)
        | Error::Config(_)
        | Error::Other(_)
        | Error::InvalidSessionName(_)
        | Error::AlreadyExists(_) => close_code::INTERNAL_ERROR,
    }
}

async fn serve_attached(
    state: Arc<AppState>,
    socket: WebSocket,
    name: SessionName,
    generation: crate::Generation,
    size: WindowSize,
    since: Option<u64>,
    peer: String,
) {
    let attached =
        state
            .manager
            .attach_with_replay(&name, generation, size, Some(peer.as_str()), since);
    let (connection, stream_offset) = match attached {
        Ok(pair) => pair,
        Err(error) => {
            let code = attach_failure_close_code(&error);
            tracing::info!(session = %name, %error, code, "attach refused after upgrade");
            close_immediately(socket, code, "attach refused").await;
            return;
        }
    };
    let ping_interval = state.manager.policy().ping_interval;
    let (mut sink, mut stream) = socket.split();
    let outbound = connection.outbound.clone();

    // One task owns the sink. Everything that needs to write goes through this
    // channel, so no two paths can race on the socket and no path has to cancel
    // an `Outbound::next()` await (cancelling it could drop a `Notify` permit and
    // stall the writer).
    let (frames_tx, mut frames_rx) = tokio::sync::mpsc::channel::<Message>(16);
    let writer = tokio::spawn(async move {
        while let Some(message) = frames_rx.recv().await {
            let closing = matches!(message, Message::Close(_));
            if sink.send(message).await.is_err() {
                break;
            }
            if closing {
                break;
            }
        }
        let _ = sink.close().await;
    });

    // Pump: session outbound → wire frames. Owns no locks and never cancels its
    // await, so a `Close` is always delivered.
    let pump_tx = frames_tx.clone();
    let pump_outbound = outbound.clone();
    let pump = tokio::spawn(async move {
        while let Some(item) = pump_outbound.next().await {
            let message = match item {
                OutboundItem::Bytes(bytes) => Message::Binary(bytes.into()),
                OutboundItem::Control(event) => {
                    // The attach notice carries the stream offset live delivery
                    // starts at, so a client can maintain its own cursor across
                    // a gap it did not choose.
                    let extra: Vec<(&str, Value)> = match &event {
                        ControlEvent::AttachNotice { .. } => {
                            vec![("stream_offset", json!(stream_offset))]
                        }
                        _ => Vec::new(),
                    };
                    Message::Text(control_frame(&event, &extra).into())
                }
                OutboundItem::Close(code) => Message::Close(Some(CloseFrame {
                    code,
                    reason: close_reason(code).into(),
                })),
            };
            let closing = matches!(message, Message::Close(_));
            if pump_tx.send(message).await.is_err() {
                break;
            }
            if closing {
                break;
            }
        }
    });

    let exit = read_loop(&state, &mut stream, &connection, &frames_tx, ping_interval).await;

    // Terminate the write path deterministically. `close` only sets a code when
    // none is set, so a lifecycle code already chosen by the session (takeover,
    // TTL, renew, shutdown) always wins over this fallback.
    match exit {
        ReadExit::Detached => connection.detach(),
        ReadExit::SocketGone => connection.detach(),
        ReadExit::Backpressure | ReadExit::FrameAbuse => {}
    }
    outbound.close(exit.close_code());
    drop(frames_tx);
    let _ = pump.await;
    let _ = writer.await;
    state.audit.record(
        AuditEvent::new(AuditKind::Detach)
            .session(&name, generation)
            .source(peer)
            .detail(exit.label()),
    );
}

/// Why the read loop ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadExit {
    /// Client asked to detach, or the socket closed / went half-open.
    Detached,
    SocketGone,
    /// The client outran the input watermark toward the PTY master.
    Backpressure,
    /// Malformed or oversized frames past the tolerance.
    FrameAbuse,
}

impl ReadExit {
    fn close_code(self) -> u16 {
        match self {
            Self::Detached | Self::SocketGone => WS_NORMAL_CLOSURE,
            // Input flooding is an admission bound, which is exactly what
            // `CAPACITY` names. `SLOW_CLIENT` is reserved for the *outbound*
            // watermark and would misreport the direction.
            Self::Backpressure => close_code::CAPACITY,
            Self::FrameAbuse => WS_POLICY_VIOLATION,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Detached => "client_detach",
            Self::SocketGone => "socket_closed",
            Self::Backpressure => "input_backpressure_disconnect",
            Self::FrameAbuse => "frame_abuse_disconnect",
        }
    }
}

fn close_reason(code: u16) -> &'static str {
    match code {
        close_code::TTL_EXPIRED => "session expired",
        close_code::ADMIN_KILL => "session killed by an operator",
        close_code::INTERNAL_ERROR => "runtime error: not retryable by re-creating",
        close_code::TAKEOVER => "another connection took over",
        close_code::RENEWED => "attach token renewed",
        close_code::SESSION_ENDED => "session ended",
        close_code::SLOW_CLIENT => "client could not keep up",
        // Named separately from every other code so a client can say "your task
        // was replaced, the workspace was reset" instead of surfacing a generic
        // network error.
        close_code::RUNTIME_REPLACED => "runtime replaced; ephemeral workspace was reset",
        close_code::CAPACITY => "capacity or admission bound reached",
        WS_POLICY_VIOLATION => "protocol violation",
        _ => "closed",
    }
}

async fn close_immediately(mut socket: WebSocket, code: u16, reason: &str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.to_string().into(),
        })))
        .await;
}

async fn read_loop(
    state: &Arc<AppState>,
    stream: &mut futures_util::stream::SplitStream<WebSocket>,
    connection: &AttachedConnection,
    frames: &tokio::sync::mpsc::Sender<Message>,
    ping_interval: Duration,
) -> ReadExit {
    let mut malformed: u32 = 0;
    let mut awaiting_pong = false;
    let mut ping = tokio::time::interval(ping_interval);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Consume the immediate first tick so the first ping is one interval away.
    ping.tick().await;

    loop {
        tokio::select! {
            incoming = stream.next() => {
                let Some(Ok(message)) = incoming else {
                    return ReadExit::SocketGone;
                };
                match message {
                    Message::Binary(bytes) => {
                        if bytes.len() > MAX_INPUT_FRAME_BYTES {
                            AbuseMetrics::bump(&state.abuse.oversize_frames);
                            return ReadExit::FrameAbuse;
                        }
                        match connection.write_input(&bytes) {
                            Ok(()) => {}
                            Err(InputError::Backpressure) => {
                                AbuseMetrics::bump(
                                    &state.abuse.input_backpressure_disconnects,
                                );
                                return ReadExit::Backpressure;
                            }
                            // Evicted: a takeover or teardown already chose this
                            // connection's close code.
                            Err(InputError::Evicted) => return ReadExit::SocketGone,
                        }
                    }
                    Message::Text(text) => {
                        if text.len() > MAX_CONTROL_FRAME_BYTES {
                            AbuseMetrics::bump(&state.abuse.oversize_frames);
                            return ReadExit::FrameAbuse;
                        }
                        match parse_client_control(text.as_str()) {
                            Ok(ClientControl::Resize(size)) => {
                                if connection.resize(size).is_err() {
                                    return ReadExit::SocketGone;
                                }
                            }
                            Ok(ClientControl::Ping) => {
                                connection.on_pong();
                                if frames
                                    .send(Message::Text(pong_frame().into()))
                                    .await
                                    .is_err()
                                {
                                    return ReadExit::SocketGone;
                                }
                            }
                            Ok(ClientControl::Detach) => return ReadExit::Detached,
                            Err(reject) => {
                                AbuseMetrics::bump(&state.abuse.malformed_frames);
                                malformed += 1;
                                tracing::debug!(?reject, malformed, "rejected control frame");
                                if malformed >= MAX_MALFORMED_FRAMES {
                                    return ReadExit::FrameAbuse;
                                }
                            }
                        }
                    }
                    Message::Pong(_) => {
                        awaiting_pong = false;
                        connection.on_pong();
                    }
                    // axum answers protocol pings itself; treat as liveness.
                    Message::Ping(_) => connection.on_pong(),
                    Message::Close(_) => return ReadExit::Detached,
                }
            }
            _ = ping.tick() => {
                if awaiting_pong && connection.on_missed_ping() {
                    // The session now counts this socket as detached; it evicted
                    // the connection itself, so stop reading it.
                    return ReadExit::SocketGone;
                }
                awaiting_pong = true;
                if frames
                    .send(Message::Ping(Default::default()))
                    .await
                    .is_err()
                {
                    return ReadExit::SocketGone;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Remote admin plane
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateRequest {
    name: String,
    rows: Option<u16>,
    cols: Option<u16>,
}

#[derive(Debug, Serialize)]
struct CredentialResponse {
    session: String,
    generation: u64,
    /// Returned **only** here, to the external control client. It is never
    /// written to a PTY master, which is what keeps it out of scrollback and out
    /// of every attached/replaying client.
    token: String,
    token_expires_in_secs: u64,
}

fn credential_response(mut credential: crate::session::SessionCredential) -> Response {
    let token = String::from_utf8_lossy(credential.token.plaintext.as_bytes()).into_owned();
    let expires_in = credential
        .token
        .grant
        .expires_at
        .saturating_duration_since(Instant::now());
    credential.token.plaintext.zeroize();
    (
        StatusCode::OK,
        Json(CredentialResponse {
            session: credential.session.as_str().to_owned(),
            generation: credential.generation.0,
            token,
            token_expires_in_secs: expires_in.as_secs(),
        }),
    )
        .into_response()
}

fn error_response(error: &Error) -> Response {
    let (status, message) = match error {
        Error::InvalidSessionName(_) => (StatusCode::BAD_REQUEST, error.to_string()),
        Error::NotFound(_) => (StatusCode::NOT_FOUND, error.to_string()),
        Error::AlreadyExists(_) => (StatusCode::CONFLICT, error.to_string()),
        Error::SessionDead(_) => (StatusCode::CONFLICT, error.to_string()),
        Error::CapacityExceeded { .. } => (StatusCode::SERVICE_UNAVAILABLE, error.to_string()),
        Error::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
        Error::Config(_) | Error::Io(_) | Error::Other(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal error".to_string(),
        ),
    };
    (status, Json(json!({ "error": message }))).into_response()
}

/// Gate an admin request: `Some(response)` is the rejection to return, `None`
/// means the credential verified.
///
/// Every failure path is audited by [`AdminAuthenticator`] itself, so this only
/// translates the outcome to HTTP.
fn admin_gate(state: &Arc<AppState>, headers: &HeaderMap, peer: &SocketAddr) -> Option<Response> {
    // Keyed by the peer *address*, not `peer.to_string()`: HTTP gives every
    // request a fresh ephemeral port, so an address:port key would put each
    // attempt in its own bucket and throttle nothing. Same rule and same caveats
    // as `UpgradeFailureLimiter` — `X-Forwarded-For` is client-settable and so is
    // never consulted, which means behind a proxy this degrades to per-proxy.
    let source = peer.ip().to_string();
    let Some(mut presented) = admin_credential(headers) else {
        // A request with no usable credential goes through the *same* per-source
        // throttle and audit path as a wrong one. Returning 401 straight from here
        // made omitting the header a free, unthrottled retry — so neither the
        // backoff nor the concurrency cap ever engaged for the cheapest way there
        // is to hammer this endpoint.
        AbuseMetrics::bump(&state.abuse.admin_auth_failures);
        return Some(admin_failure_response(
            state.admin.reject_unauthenticated(Some(&source)),
        ));
    };
    match state.admin.verify(&mut presented, Some(&source)) {
        Ok(()) => None,
        Err(failure) => {
            AbuseMetrics::bump(&state.abuse.admin_auth_failures);
            Some(admin_failure_response(failure))
        }
    }
}

fn admin_failure_response(failure: AdminAuthFailure) -> Response {
    match failure {
        AdminAuthFailure::Invalid => StatusCode::UNAUTHORIZED.into_response(),
        AdminAuthFailure::TooLarge => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        AdminAuthFailure::Busy => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        AdminAuthFailure::Throttled { retry_after } => (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", retry_after.as_secs().max(1).to_string())],
        )
            .into_response(),
    }
}

async fn admin_list(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = admin_gate(&state, &headers, &peer) {
        return response;
    }
    let metrics = state.manager.metrics();
    Json(json!({
        "sessions": state.manager.list(),
        // Carries `teardown_best_effort` / `absolute_ttl_best_effort`: the
        // guarantee is labelled everywhere it is surfaced, never implied.
        "kill_domain": state.manager.kill_domain().status(),
        "draining": state.is_draining(),
        "metrics": {
            "sessions_created": metrics.sessions_created.load(Ordering::Relaxed),
            "admission_rejected": metrics.admission_rejected.load(Ordering::Relaxed),
            "takeovers": metrics.takeovers.load(Ordering::Relaxed),
            "takeovers_rate_limited": metrics.takeovers_rate_limited.load(Ordering::Relaxed),
            "ttl_expired": metrics.ttl_expired.load(Ordering::Relaxed),
            "self_exits": metrics.self_exits.load(Ordering::Relaxed),
        },
        "abuse": state.abuse.snapshot(),
    }))
    .into_response()
}

async fn admin_create(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<CreateRequest>,
) -> Response {
    if let Some(response) = admin_gate(&state, &headers, &peer) {
        return response;
    }
    if state.is_draining() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime is draining for shutdown",
        )
            .into_response();
    }
    let name = match SessionName::parse(&request.name) {
        Ok(name) => name,
        Err(error) => return error_response(&error),
    };
    let size = match WindowSize::validated(request.rows.unwrap_or(24), request.cols.unwrap_or(80)) {
        Ok(size) => size,
        Err(error) => return error_response(&error),
    };
    match state.manager.create(name, size) {
        Ok(credential) => credential_response(credential),
        Err(error) => error_response(&error),
    }
}

async fn admin_renew(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(session): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = admin_gate(&state, &headers, &peer) {
        return response;
    }
    let name = match SessionName::parse(&session) {
        Ok(name) => name,
        Err(error) => return error_response(&error),
    };
    match state.manager.renew(&name) {
        Ok(credential) => credential_response(credential),
        Err(error) => error_response(&error),
    }
}

async fn admin_restart(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(session): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = admin_gate(&state, &headers, &peer) {
        return response;
    }
    let name = match SessionName::parse(&session) {
        Ok(name) => name,
        Err(error) => return error_response(&error),
    };
    match state.manager.restart_in_place(&name).await {
        Ok(credential) => credential_response(credential),
        Err(error) => error_response(&error),
    }
}

async fn admin_kill(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(session): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = admin_gate(&state, &headers, &peer) {
        return response;
    }
    let name = match SessionName::parse(&session) {
        Ok(name) => name,
        Err(error) => return error_response(&error),
    };
    match state.manager.kill(&name).await {
        Ok(report) => Json(report).into_response(),
        Err(error) => error_response(&error),
    }
}

// ---------------------------------------------------------------------------
// Bind and serve
// ---------------------------------------------------------------------------

/// Bind the listener after the fail-closed guard.
pub async fn bind(
    listen: &str,
    admin_credential_hash: &str,
    tls_terminated_upstream: bool,
) -> Result<tokio::net::TcpListener, Error> {
    listener_guard(listen, admin_credential_hash, tls_terminated_upstream)
        .map_err(Error::Config)?;
    tokio::net::TcpListener::bind(listen)
        .await
        .map_err(Error::Io)
}

/// Serve until `shutdown` resolves, then drain.
///
/// Shutdown order is load-bearing and matches the ADR's ephemerality
/// requirement: mark draining (so no new work arrives) → notify attached clients
/// → wait out the grace → tear every session down, which closes each socket with
/// `RUNTIME_REPLACED` so a client reports a replaced runtime instead of a
/// generic network error.
pub async fn serve<S>(
    state: Arc<AppState>,
    listener: tokio::net::TcpListener,
    shutdown: S,
) -> Result<(), Error>
where
    S: std::future::Future<Output = ()> + Send + 'static,
{
    let ticker = spawn_ticker(state.clone());

    let drain_state = state.clone();
    let graceful = async move {
        shutdown.await;
        drain_state.begin_drain();
        let grace = drain_state.config.drain_grace;
        tracing::info!(
            grace_secs = grace.as_secs(),
            "draining: notifying attached clients before teardown"
        );
        drain_state.manager.notify_shutdown(grace);
        tokio::time::sleep(grace).await;
        let reports = drain_state.manager.shutdown().await;
        for report in &reports {
            tracing::info!(
                session = %report.session,
                survivors = report.kill_domain.survivors.len(),
                best_effort = report.kill_domain.is_best_effort(),
                "session torn down for shutdown"
            );
        }
    };

    let result = axum::serve(
        NoDelayListener(listener),
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(graceful)
    .await
    .map_err(Error::Io);

    ticker.abort();
    result
}

/// A `TcpListener` that disables Nagle's algorithm on every accepted connection.
///
/// This is a latency fix, not a tuning knob. An interactive terminal writes one
/// keystroke echo at a time, which is exactly the traffic Nagle holds back while
/// it waits for more data to coalesce; combined with the peer's delayed ACK it
/// quantises every round trip. Measured against a tailnet peer whose raw RTT is
/// ~5 ms, echo latency was a median of 81 ms and a maximum of 133 ms, in the
/// characteristic bimodal pattern. It also reads as dropped input rather than
/// slow input: type two characters quickly and the second appears late enough
/// that it looks eaten.
///
/// Found by a human typing into the thing, not by any test here -- no unit or
/// end-to-end assertion in this crate would notice, because they all measure
/// what arrives and never how long it took.
struct NoDelayListener(tokio::net::TcpListener);

impl axum::serve::Listener for NoDelayListener {
    type Io = tokio::net::TcpStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            match self.0.accept().await {
                Ok((stream, addr)) => {
                    // Best effort: a peer that cannot take the option is still
                    // worth serving, just with the latency described above.
                    if let Err(error) = stream.set_nodelay(true) {
                        tracing::warn!(%addr, %error, "could not disable Nagle for this peer");
                    }
                    return (stream, addr);
                }
                Err(error) => {
                    tracing::warn!(%error, "accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.0.local_addr()
    }
}

fn spawn_ticker(state: Arc<AppState>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(state.config.tick_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            for action in state.manager.tick().await {
                tracing::info!(?action, "ttl maintenance");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (key, value) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(key.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn bearer_comes_from_authorization_or_the_browser_subprotocol_only() {
        assert_eq!(
            ws_bearer_token(&headers(&[("authorization", "Bearer abc123")])),
            Some("abc123")
        );
        assert_eq!(
            ws_bearer_token(&headers(&[(
                "sec-websocket-protocol",
                "openab.bearer.abc123, openab.pty.v1"
            )])),
            Some("abc123")
        );
        // Authorization wins when both are present, as on `/acp`.
        assert_eq!(
            ws_bearer_token(&headers(&[
                ("authorization", "Bearer from-header"),
                ("sec-websocket-protocol", "openab.bearer.from-subprotocol"),
            ])),
            Some("from-header")
        );
        // No query-parameter path exists at all, and an unrelated subprotocol
        // offer is not a credential.
        assert_eq!(
            ws_bearer_token(&headers(&[("sec-websocket-protocol", "openab.pty.v1")])),
            None
        );
        assert_eq!(ws_bearer_token(&HeaderMap::new()), None);
    }

    #[test]
    fn admin_credential_is_header_only_and_bounded() {
        assert!(admin_credential(&headers(&[("authorization", "Bearer secret")])).is_some());
        assert!(admin_credential(&headers(&[("authorization", "Bearer ")])).is_none());
        assert!(admin_credential(&headers(&[("authorization", "Basic secret")])).is_none());
        let long = format!("Bearer {}", "a".repeat(MAX_ADMIN_CREDENTIAL_BYTES + 1));
        assert!(admin_credential(&headers(&[("authorization", &long)])).is_none());
    }

    #[test]
    fn origin_is_never_consulted_for_authorization() {
        // A regression guard for a decided property: the attach surface's auth
        // inputs are the bearer sources only. If someone adds an Origin check,
        // this list is where it would have to appear.
        let with_origin = headers(&[
            ("authorization", "Bearer abc"),
            ("origin", "https://evil.example"),
        ]);
        assert_eq!(ws_bearer_token(&with_origin), Some("abc"));
    }

    #[test]
    fn control_frames_are_a_strict_allowlist() {
        assert_eq!(
            parse_client_control(r#"{"v":1,"type":"resize","rows":40,"cols":120}"#),
            Ok(ClientControl::Resize(
                WindowSize::validated(40, 120).unwrap()
            ))
        );
        assert_eq!(
            parse_client_control(r#"{"v":1,"type":"ping"}"#),
            Ok(ClientControl::Ping)
        );
        assert_eq!(
            parse_client_control(r#"{"v":1,"type":"detach"}"#),
            Ok(ClientControl::Detach)
        );

        assert_eq!(
            parse_client_control(r#"{"type":"ping"}"#),
            Err(FrameReject::Version)
        );
        assert_eq!(
            parse_client_control(r#"{"v":2,"type":"ping"}"#),
            Err(FrameReject::Version)
        );
        assert_eq!(
            parse_client_control(r#"{"v":1,"type":"exec","cmd":"sh"}"#),
            Err(FrameReject::UnknownType)
        );
        assert_eq!(parse_client_control("not json"), Err(FrameReject::NotJson));
        assert_eq!(parse_client_control("[1,2,3]"), Err(FrameReject::NotJson));
        for bad in [
            r#"{"v":1,"type":"resize","rows":0,"cols":80}"#,
            r#"{"v":1,"type":"resize","rows":40,"cols":100000}"#,
            r#"{"v":1,"type":"resize","rows":-1,"cols":80}"#,
            r#"{"v":1,"type":"resize","rows":"40","cols":"80"}"#,
            r#"{"v":1,"type":"resize"}"#,
        ] {
            assert_eq!(
                parse_client_control(bad),
                Err(FrameReject::ResizeBounds),
                "must reject {bad}"
            );
        }
        let oversize = format!(
            r#"{{"v":1,"type":"ping","pad":"{}"}}"#,
            "x".repeat(MAX_CONTROL_FRAME_BYTES)
        );
        assert_eq!(parse_client_control(&oversize), Err(FrameReject::Oversize));
    }

    #[test]
    fn server_control_frames_are_versioned() {
        let notice = control_frame(
            &ControlEvent::AttachNotice {
                ephemeral_workspace: true,
                teardown_best_effort: true,
                externalise_with: "git push",
            },
            &[("stream_offset", json!(42))],
        );
        let value: Value = serde_json::from_str(&notice).unwrap();
        assert_eq!(value["v"], json!(CONTROL_SCHEMA_VERSION));
        assert_eq!(value["type"], json!("attach-notice"));
        // Ephemerality is stated in the product surface, not left to discovery.
        assert_eq!(value["ephemeral_workspace"], json!(true));
        assert_eq!(value["teardown_best_effort"], json!(true));
        assert_eq!(value["stream_offset"], json!(42));

        let ttl = control_frame(
            &ControlEvent::TtlWarning {
                seconds_remaining: 30,
                absolute: true,
            },
            &[],
        );
        let value: Value = serde_json::from_str(&ttl).unwrap();
        assert_eq!(value["type"], json!("ttl-warning"));
        assert_eq!(value["v"], json!(CONTROL_SCHEMA_VERSION));

        let pong: Value = serde_json::from_str(&pong_frame()).unwrap();
        assert_eq!(pong["type"], json!("pong"));
        assert_eq!(pong["v"], json!(CONTROL_SCHEMA_VERSION));
    }

    #[test]
    fn runtime_replaced_has_its_own_client_visible_reason() {
        // Distinct from TTL expiry, takeover, and self-exit, so a client can say
        // "your task was replaced" rather than showing a network error.
        let replaced = close_reason(close_code::RUNTIME_REPLACED);
        assert!(replaced.contains("replaced"));
        for other in [
            close_code::TTL_EXPIRED,
            close_code::TAKEOVER,
            close_code::SESSION_ENDED,
        ] {
            assert_ne!(close_reason(other), replaced);
        }
    }

    #[test]
    fn an_admin_kill_is_not_reported_as_an_expiry() {
        // Same finding on both surfaces: an operator kill used to reuse the TTL
        // close code, so the client told its user "session expired" about a kill
        // the operator had just issued, and audit could not separate the two.
        assert_ne!(close_code::ADMIN_KILL, close_code::TTL_EXPIRED);
        assert_ne!(
            close_reason(close_code::ADMIN_KILL),
            close_reason(close_code::TTL_EXPIRED)
        );
    }

    #[test]
    fn internal_faults_do_not_close_with_a_retry_create_hint() {
        // CAPACITY tells a client "full, try creating again". An IO/config/other
        // fault is not that, and saying so produced a create loop on a fault.
        for internal in [
            Error::Io(std::io::Error::other("synthetic")),
            Error::Config("synthetic".into()),
            Error::Other("synthetic".into()),
        ] {
            assert_eq!(
                attach_failure_close_code(&internal),
                close_code::INTERNAL_ERROR,
                "{internal} must not be reported as capacity"
            );
        }
        assert_eq!(
            attach_failure_close_code(&Error::CapacityExceeded { limit: 1 }),
            close_code::CAPACITY
        );
    }

    #[test]
    fn listener_refuses_off_loopback_without_auth_material_or_a_terminator() {
        assert!(listener_guard("127.0.0.1:8090", "", false).is_ok());
        assert!(listener_guard("localhost:8090", "", false).is_ok());
        assert!(listener_guard("[::1]:8090", "", false).is_ok());

        assert!(listener_guard("0.0.0.0:8090", "", true).is_err());
        assert!(listener_guard("0.0.0.0:8090", "sha256:aa", false).is_err());
        assert!(listener_guard("0.0.0.0:8090", "sha256:aa", true).is_ok());
        // Unparseable host is treated as non-loopback (fail safe).
        assert!(listener_guard("pty.internal:8090", "", true).is_err());
    }

    #[test]
    fn upgrade_failures_ban_a_source_then_expire() {
        let limiter =
            UpgradeFailureLimiter::new(3, Duration::from_secs(60), Duration::from_secs(30));
        let source: IpAddr = "203.0.113.7".parse().unwrap();
        let start = Instant::now();
        assert!(limiter.check_at(source, start).is_ok());
        assert!(!limiter.record_failure_at(source, start));
        assert!(!limiter.record_failure_at(source, start));
        assert!(
            limiter.record_failure_at(source, start),
            "the limit-th failure starts the ban"
        );
        assert!(limiter.check_at(source, start).is_err());
        assert!(limiter
            .check_at(source, start + Duration::from_secs(31))
            .is_ok());
        // An unrelated source is unaffected.
        let other: IpAddr = "203.0.113.8".parse().unwrap();
        assert!(limiter.check_at(other, start).is_ok());
    }

    #[test]
    fn failures_outside_the_window_do_not_accumulate() {
        let limiter =
            UpgradeFailureLimiter::new(3, Duration::from_secs(60), Duration::from_secs(30));
        let source: IpAddr = "203.0.113.9".parse().unwrap();
        let start = Instant::now();
        assert!(!limiter.record_failure_at(source, start));
        assert!(!limiter.record_failure_at(source, start + Duration::from_secs(61)));
        assert!(!limiter.record_failure_at(source, start + Duration::from_secs(62)));
        assert!(limiter
            .check_at(source, start + Duration::from_secs(62))
            .is_ok());
    }

    #[test]
    fn a_successful_attach_clears_the_bucket() {
        let limiter = UpgradeFailureLimiter::default();
        let source: IpAddr = "198.51.100.4".parse().unwrap();
        limiter.record_failure(source);
        limiter.on_success(source);
        assert!(limiter.check(source).is_ok());
    }

    #[test]
    fn distinct_source_churn_cannot_evict_an_active_ban() {
        // The map is bounded, so it evicts. Eviction must not be a way to shake a
        // ban off: `check` refreshes `last_seen` even while banned (so a knocking
        // source is not the LRU victim), and a live ban is never a candidate.
        let limiter =
            UpgradeFailureLimiter::new(2, Duration::from_secs(600), Duration::from_secs(600));
        let banned: IpAddr = "203.0.113.10".parse().unwrap();
        let start = Instant::now();
        assert!(!limiter.record_failure_at(banned, start));
        assert!(
            limiter.record_failure_at(banned, start),
            "the ban starts here"
        );
        assert!(limiter.check_at(banned, start).is_err());

        // `check` must refresh liveness even while banned, or a source that keeps
        // knocking is the *stalest* entry in the map and the first one evicted.
        let before = limiter.entries.lock().get(&banned).unwrap().last_seen;
        let later = start + Duration::from_secs(5);
        assert!(limiter.check_at(banned, later).is_err());
        let after = limiter.entries.lock().get(&banned).unwrap().last_seen;
        assert!(
            after > before,
            "a banned source that keeps knocking must not look stale to LRU eviction"
        );

        // Now churn distinct addresses past the map bound. Each records a single
        // failure, so they are evictable; the ban must not be what gets evicted.
        for index in 0..(MAX_TRACKED_SOURCES + 64) {
            let churn = IpAddr::from(std::net::Ipv6Addr::from(index as u128 + 1));
            limiter.record_failure_at(churn, later);
            let _ = limiter.check_at(banned, later);
        }
        assert!(
            limiter.entries.lock().len() <= MAX_TRACKED_SOURCES,
            "the tracking map must stay bounded"
        );
        assert!(
            limiter.check_at(banned, later).is_err(),
            "an active ban must survive churn across distinct addresses"
        );
    }

    #[test]
    fn read_exit_codes_separate_protocol_abuse_from_lifecycle() {
        assert_eq!(ReadExit::Detached.close_code(), WS_NORMAL_CLOSURE);
        assert_eq!(ReadExit::FrameAbuse.close_code(), WS_POLICY_VIOLATION);
        assert_eq!(ReadExit::Backpressure.close_code(), close_code::CAPACITY);
        // The outbound watermark code must not be reused for input flooding.
        assert_ne!(ReadExit::Backpressure.close_code(), close_code::SLOW_CLIENT);
    }

    #[test]
    fn attach_failures_map_to_distinguishable_close_codes() {
        let name = SessionName::parse("s").unwrap();
        assert_eq!(
            attach_failure_close_code(&Error::SessionDead(name.clone())),
            close_code::SESSION_ENDED
        );
        assert_eq!(
            attach_failure_close_code(&Error::NotFound(name)),
            close_code::SESSION_ENDED
        );
        assert_eq!(
            attach_failure_close_code(&Error::Unauthorized),
            close_code::RENEWED
        );
        assert_eq!(
            attach_failure_close_code(&Error::CapacityExceeded { limit: 3 }),
            close_code::CAPACITY
        );
    }

    #[test]
    fn admin_errors_map_to_distinct_statuses() {
        let name = SessionName::parse("s").unwrap();
        let status = |error: &Error| error_response(error).status();
        assert_eq!(
            status(&Error::AlreadyExists(name.clone())),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status(&Error::NotFound(name.clone())),
            StatusCode::NOT_FOUND
        );
        assert_eq!(status(&Error::SessionDead(name)), StatusCode::CONFLICT);
        assert_eq!(
            status(&Error::CapacityExceeded { limit: 1 }),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(status(&Error::Unauthorized), StatusCode::UNAUTHORIZED);
        assert_eq!(
            status(&Error::Other("x".into())),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}

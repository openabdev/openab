//! End-to-end tests over a real listener, a real WebSocket client, and real PTY
//! children.
//!
//! Every test here is `#[ignore]`d on purpose: they spawn processes and bind
//! sockets, which the crate's unit tests deliberately never do. Run them with
//! `cargo test -p openab-pty -- --ignored`.
//!
//! The harness always calls [`Harness::shutdown`], which drains through the same
//! path the SIGTERM handler uses. That is not tidiness: without it each test
//! would leave a `/bin/sh` child behind.

use openab_pty::admin_auth::AdminAuthenticator;
use openab_pty::audit::AuditLogger;
use openab_pty::close_code;
use openab_pty::config;
use openab_pty::killdomain::{self, KillDomain, TrackingLimits};
use openab_pty::server::{self, AppState, ServerConfig};
use openab_pty::session::{PortablePtySpawner, PtySpawner, SessionManager, SessionPolicy};
use openab_pty::token::TokenStore;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite;

/// Generous on purpose: these budgets guard against a hang, they do not measure
/// anything, so a loaded machine must not turn them into flakes.
const IO_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    addr: SocketAddr,
    admin_credential: String,
    manager: Arc<SessionManager>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    served: Option<tokio::task::JoinHandle<()>>,
}

fn projection(command: &str, verifier: &str, max_sessions: usize) -> String {
    format!(
        r#"[pty]
enabled = true
listen = "127.0.0.1:0"
tls_terminated_upstream = true
command = "{command}"
max_sessions = {max_sessions}
absolute_session_ttl = "12h"
scrollback_kib = 1024
scrollback_replay = false
admin_credential_hash = "{verifier}"
"#
    )
}

fn test_command() -> &'static str {
    "/bin/sh"
}

impl Harness {
    async fn start(token_ttl: Duration) -> Self {
        let audit = AuditLogger;
        let generated = AdminAuthenticator::generate();
        let admin_credential =
            String::from_utf8(generated.plaintext.as_bytes().to_vec()).expect("hex credential");
        let parsed =
            config::validate_projection(&projection(test_command(), &generated.verifier, 4))
                .expect("the test projection must validate");

        let spawner = Arc::new(PortablePtySpawner);
        let capability = spawner.capability();
        let selection =
            killdomain::detect(parsed.kill_domain_requirement, capability).expect("tier selection");
        let kill = Arc::new(KillDomain::new(
            selection,
            TrackingLimits::default(),
            audit.clone(),
        ));
        let tokens = TokenStore::new(token_ttl, audit.clone());
        let verifier = tokens.attach_verifier();
        // A tiny backoff keeps the real throttle path in play without making the
        // test wait out an exponential one; the backoff curve itself is unit
        // tested in `admin_auth`.
        let admin = AdminAuthenticator::with_limits(
            &generated.verifier,
            4,
            Duration::from_millis(1),
            audit.clone(),
        )
        .expect("admin authenticator");
        let policy = SessionPolicy::from_config(&parsed);
        let manager = SessionManager::new(parsed, policy, tokens, audit.clone(), kill, spawner)
            .expect("session manager");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        let state = AppState::new(
            manager.clone(),
            verifier,
            admin,
            audit,
            ServerConfig {
                listen: addr.to_string(),
                tls_terminated_upstream: true,
                // No drain wait in tests: the notice path is exercised, the sleep
                // is not what is under test.
                drain_grace: Duration::ZERO,
                tick_interval: Duration::from_secs(1),
                workspace: std::env::temp_dir(),
                volume_capacity_kib: 0,
            },
        );
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let served = tokio::spawn(async move {
            let _ = server::serve(state, listener, async move {
                let _ = stopped.await;
            })
            .await;
        });
        Self {
            addr,
            admin_credential,
            manager,
            stop: Some(stop),
            served: Some(served),
        }
    }

    async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(served) = self.served.take() {
            let _ = tokio::time::timeout(IO_TIMEOUT, served).await;
        }
        // Belt and braces: if the listener task was already gone, tear down
        // directly so no child outlives the test.
        let _ = self.manager.shutdown().await;
    }

    async fn admin(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> (u16, serde_json::Value) {
        self.request(method, path, Some(self.admin_credential.as_str()), body)
            .await
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        credential: Option<&str>,
        body: Option<&str>,
    ) -> (u16, serde_json::Value) {
        let (status, raw) = http_request(self.addr, method, path, credential, body).await;
        let parsed = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
        (status, parsed)
    }

    async fn create(&self, name: &str) -> String {
        let (status, body) = self
            .admin(
                "POST",
                "/admin/sessions",
                Some(&format!(r#"{{"name":"{name}","rows":24,"cols":80}}"#)),
            )
            .await;
        assert_eq!(status, 200, "create failed: {body}");
        body["token"]
            .as_str()
            .expect("token in response")
            .to_owned()
    }
}

// ---------------------------------------------------------------------------
// Minimal HTTP/1.1 client
// ---------------------------------------------------------------------------

/// Hand-rolled so the crate needs no HTTP client dependency for four assertions.
/// `Connection: close` makes the read terminate at EOF, which keeps a broken
/// response from turning into a hang.
async fn http_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    credential: Option<&str>,
    body: Option<&str>,
) -> (u16, String) {
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if let Some(credential) = credential {
        request.push_str(&format!("Authorization: Bearer {credential}\r\n"));
    }
    match body {
        Some(body) => {
            request.push_str("Content-Type: application/json\r\n");
            request.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
            request.push_str(body);
        }
        None => request.push_str("\r\n"),
    }

    let mut stream = tokio::time::timeout(IO_TIMEOUT, tokio::net::TcpStream::connect(addr))
        .await
        .expect("connect timed out")
        .expect("connect");
    tokio::time::timeout(IO_TIMEOUT, stream.write_all(request.as_bytes()))
        .await
        .expect("write timed out")
        .expect("write");
    let mut raw = Vec::new();
    tokio::time::timeout(IO_TIMEOUT, stream.read_to_end(&mut raw))
        .await
        .expect("read timed out")
        .expect("read");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)
    } else {
        body.to_string()
    };
    (status, body)
}

fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size, tail)) = rest.split_once("\r\n") {
        let Ok(size) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if size == 0 || tail.len() < size {
            break;
        }
        out.push_str(&tail[..size]);
        rest = tail[size..].trim_start_matches("\r\n");
    }
    out
}

// ---------------------------------------------------------------------------
// WebSocket client
// ---------------------------------------------------------------------------

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

enum Attach {
    Connected(Box<Socket>),
    Refused(u16),
}

/// Attach with `Authorization: Bearer` (the non-browser path).
async fn attach_with_header(addr: SocketAddr, session: &str, token: &str) -> Attach {
    connect(
        addr,
        session,
        Some(("authorization", format!("Bearer {token}"))),
    )
    .await
}

/// Attach with the browser path: browsers cannot set `Authorization` on an
/// upgrade, so the token rides `Sec-WebSocket-Protocol`.
async fn attach_with_subprotocol(addr: SocketAddr, session: &str, token: &str) -> Attach {
    connect(
        addr,
        session,
        Some((
            "sec-websocket-protocol",
            format!("openab.bearer.{token}, openab.pty.v1"),
        )),
    )
    .await
}

async fn connect(addr: SocketAddr, session: &str, header: Option<(&str, String)>) -> Attach {
    connect_url(&format!("ws://{addr}/pty/{session}"), header).await
}

async fn connect_url(url: &str, header: Option<(&str, String)>) -> Attach {
    use tungstenite::client::IntoClientRequest;

    let mut request = url.to_string().into_client_request().expect("client request");
    if let Some((name, value)) = header {
        request.headers_mut().insert(
            tungstenite::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            tungstenite::http::HeaderValue::from_str(&value).unwrap(),
        );
    }
    match tokio::time::timeout(IO_TIMEOUT, tokio_tungstenite::connect_async(request)).await {
        Err(_) => panic!("websocket handshake timed out"),
        Ok(Ok((socket, _response))) => Attach::Connected(Box::new(socket)),
        Ok(Err(tungstenite::Error::Http(response))) => Attach::Refused(response.status().as_u16()),
        Ok(Err(error)) => panic!("unexpected websocket error: {error}"),
    }
}

impl Attach {
    fn connected(self) -> Socket {
        match self {
            Self::Connected(socket) => *socket,
            Self::Refused(status) => panic!("attach refused with HTTP {status}"),
        }
    }

    fn refused_status(self) -> u16 {
        match self {
            Self::Refused(status) => status,
            Self::Connected(_) => panic!("attach should have been refused"),
        }
    }
}

/// Next text control frame, skipping PTY bytes and protocol pings.
async fn next_control(socket: &mut Socket) -> serde_json::Value {
    use futures_util::StreamExt;
    loop {
        let message = tokio::time::timeout(IO_TIMEOUT, socket.next())
            .await
            .expect("timed out waiting for a control frame")
            .expect("socket closed before a control frame")
            .expect("websocket error");
        match message {
            tungstenite::Message::Text(text) => {
                return serde_json::from_str(&text).expect("control frames are JSON")
            }
            tungstenite::Message::Close(frame) => {
                panic!("server closed instead of sending a control frame: {frame:?}")
            }
            _ => continue,
        }
    }
}

/// Drain until the server closes, returning the close code.
async fn wait_for_close(socket: &mut Socket) -> u16 {
    use futures_util::StreamExt;
    loop {
        let message = tokio::time::timeout(IO_TIMEOUT, socket.next())
            .await
            .expect("timed out waiting for close");
        match message {
            None => panic!("stream ended without a close frame"),
            Some(Err(tungstenite::Error::ConnectionClosed)) => panic!("closed without a code"),
            Some(Err(error)) => panic!("websocket error: {error}"),
            Some(Ok(tungstenite::Message::Close(Some(frame)))) => return frame.code.into(),
            Some(Ok(tungstenite::Message::Close(None))) => panic!("close frame without a code"),
            Some(Ok(_)) => continue,
        }
    }
}

// ---------------------------------------------------------------------------
// Attach authorization
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "binds a socket and spawns a PTY child"]
async fn a_valid_token_attaches_and_is_told_the_workspace_is_ephemeral() {
    let harness = Harness::start(Duration::from_secs(60)).await;
    let token = harness.create("alpha").await;

    let mut socket = attach_with_header(harness.addr, "alpha", &token)
        .await
        .connected();
    let notice = next_control(&mut socket).await;
    assert_eq!(notice["v"], serde_json::json!(1));
    assert_eq!(notice["type"], serde_json::json!("attach-notice"));
    assert_eq!(notice["ephemeral_workspace"], serde_json::json!(true));
    assert!(notice["externalise_with"].is_string());
    assert!(notice["stream_offset"].is_u64());
    // Tier 1 is the default, and the notice must say the guarantee is best effort.
    assert!(notice["teardown_best_effort"].is_boolean());

    harness.shutdown().await;
}

#[tokio::test]
#[ignore = "binds a socket and spawns a PTY child"]
async fn the_browser_subprotocol_carries_the_bearer_token() {
    let harness = Harness::start(Duration::from_secs(60)).await;
    let token = harness.create("browser").await;

    let mut socket = attach_with_subprotocol(harness.addr, "browser", &token)
        .await
        .connected();
    let notice = next_control(&mut socket).await;
    assert_eq!(notice["type"], serde_json::json!("attach-notice"));

    harness.shutdown().await;
}

#[tokio::test]
#[ignore = "binds a socket and spawns a PTY child"]
async fn a_wrong_or_missing_token_is_refused() {
    let harness = Harness::start(Duration::from_secs(60)).await;
    let _token = harness.create("beta").await;

    let wrong = "0".repeat(64);
    assert_eq!(
        attach_with_header(harness.addr, "beta", &wrong)
            .await
            .refused_status(),
        401
    );
    assert_eq!(
        connect(harness.addr, "beta", None).await.refused_status(),
        401
    );
    // An unknown session is indistinguishable from a bad token: no enumeration.
    assert_eq!(
        attach_with_header(harness.addr, "no-such-session", &wrong)
            .await
            .refused_status(),
        401
    );

    harness.shutdown().await;
}

#[tokio::test]
#[ignore = "binds a socket and spawns a PTY child"]
async fn an_expired_token_is_refused() {
    // Short attach-token TTL; the session TTL is untouched, which is the point:
    // token exposure is bounded well below session lifetime.
    let harness = Harness::start(Duration::from_millis(300)).await;
    let token = harness.create("gamma").await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    assert_eq!(
        attach_with_header(harness.addr, "gamma", &token)
            .await
            .refused_status(),
        401
    );

    harness.shutdown().await;
}

#[tokio::test]
#[ignore = "binds a socket and spawns a PTY child"]
async fn a_revoked_token_is_refused_after_kill() {
    let harness = Harness::start(Duration::from_secs(60)).await;
    let token = harness.create("delta").await;

    let (status, report) = harness.admin("DELETE", "/admin/sessions/delta", None).await;
    assert_eq!(status, 200, "kill failed: {report}");
    assert_eq!(report["class"], serde_json::json!("user_kill"));

    assert_eq!(
        attach_with_header(harness.addr, "delta", &token)
            .await
            .refused_status(),
        401
    );

    harness.shutdown().await;
}

#[tokio::test]
#[ignore = "binds a socket and spawns a PTY child"]
async fn a_generation_bump_invalidates_an_outstanding_token() {
    let harness = Harness::start(Duration::from_secs(60)).await;
    let first = harness.create("epsilon").await;

    let (status, renewed) = harness
        .admin("POST", "/admin/sessions/epsilon/renew", None)
        .await;
    assert_eq!(status, 200, "renew failed: {renewed}");
    let second = renewed["token"].as_str().expect("renewed token").to_owned();
    assert_eq!(renewed["generation"], serde_json::json!(2));
    assert_ne!(first, second, "renew must mint a fresh token");

    assert_eq!(
        attach_with_header(harness.addr, "epsilon", &first)
            .await
            .refused_status(),
        401,
        "the pre-renew token must be dead"
    );
    let mut socket = attach_with_header(harness.addr, "epsilon", &second)
        .await
        .connected();
    assert_eq!(
        next_control(&mut socket).await["type"],
        serde_json::json!("attach-notice")
    );

    harness.shutdown().await;
}

#[tokio::test]
#[ignore = "binds a socket and spawns a PTY child"]
async fn a_renew_while_attached_evicts_the_connection_with_its_own_close_code() {
    let harness = Harness::start(Duration::from_secs(60)).await;
    let token = harness.create("zeta").await;
    let mut socket = attach_with_header(harness.addr, "zeta", &token)
        .await
        .connected();
    assert_eq!(
        next_control(&mut socket).await["type"],
        serde_json::json!("attach-notice")
    );

    let (status, _) = harness
        .admin("POST", "/admin/sessions/zeta/renew", None)
        .await;
    assert_eq!(status, 200);
    // Renew-distinct, so a client can tell renewal from takeover.
    assert_eq!(wait_for_close(&mut socket).await, close_code::RENEWED);

    harness.shutdown().await;
}

#[tokio::test]
#[ignore = "binds a socket and spawns a PTY child"]
async fn a_second_attach_takes_over_the_cas_and_evicts_the_incumbent() {
    let harness = Harness::start(Duration::from_secs(60)).await;
    let token = harness.create("eta").await;

    let mut first = attach_with_header(harness.addr, "eta", &token)
        .await
        .connected();
    assert_eq!(
        next_control(&mut first).await["type"],
        serde_json::json!("attach-notice")
    );

    // A still-valid token reattaches: documented takeover behaviour, not an error.
    let mut second = attach_with_header(harness.addr, "eta", &token)
        .await
        .connected();
    assert_eq!(
        next_control(&mut second).await["type"],
        serde_json::json!("attach-notice")
    );
    assert_eq!(wait_for_close(&mut first).await, close_code::TAKEOVER);

    harness.shutdown().await;
}

#[tokio::test]
#[ignore = "binds a socket and spawns a PTY child"]
async fn restart_in_place_serves_a_reattach_to_dead_session() {
    let harness = Harness::start(Duration::from_secs(60)).await;
    let _token = harness.create("theta").await;
    let (status, _) = harness.admin("DELETE", "/admin/sessions/theta", None).await;
    assert_eq!(status, 200);

    let (status, restarted) = harness
        .admin("POST", "/admin/sessions/theta/restart", None)
        .await;
    assert_eq!(status, 200, "restart failed: {restarted}");
    let token = restarted["token"].as_str().expect("token").to_owned();
    assert!(
        restarted["generation"].as_u64().unwrap() >= 2,
        "restart-in-place must bump the generation"
    );
    let mut socket = attach_with_header(harness.addr, "theta", &token)
        .await
        .connected();
    assert_eq!(
        next_control(&mut socket).await["type"],
        serde_json::json!("attach-notice")
    );

    harness.shutdown().await;
}

// ---------------------------------------------------------------------------
// Byte plumbing, cursor reconnect, and frame abuse
// ---------------------------------------------------------------------------

/// Read binary frames until `needle` appears in the accumulated bytes.
async fn expect_output_containing(socket: &mut Socket, needle: &str) -> Vec<u8> {
    use futures_util::StreamExt;
    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + IO_TIMEOUT;
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out waiting for {needle:?}; saw {:?}",
                    String::from_utf8_lossy(&seen)
                )
            })
            .expect("socket closed")
            .expect("websocket error");
        match message {
            tungstenite::Message::Binary(bytes) => {
                seen.extend_from_slice(&bytes);
                if String::from_utf8_lossy(&seen).contains(needle) {
                    return seen;
                }
            }
            tungstenite::Message::Close(frame) => {
                panic!("server closed while waiting for {needle:?}: {frame:?}")
            }
            _ => continue,
        }
    }
}

#[tokio::test]
#[ignore = "binds a socket and spawns a PTY child"]
async fn input_reaches_the_child_and_output_replays_from_a_cursor() {
    use futures_util::SinkExt;

    let harness = Harness::start(Duration::from_secs(60)).await;
    let token = harness.create("kappa").await;
    let mut socket = attach_with_header(harness.addr, "kappa", &token)
        .await
        .connected();
    let notice = next_control(&mut socket).await;
    assert_eq!(notice["type"], serde_json::json!("attach-notice"));

    // A resize goes through the strict allowlist and reaches TIOCSWINSZ, and an
    // application-level ping is answered without touching the PTY.
    socket
        .send(tungstenite::Message::Text(
            r#"{"v":1,"type":"resize","rows":40,"cols":132}"#.into(),
        ))
        .await
        .expect("send resize");
    socket
        .send(tungstenite::Message::Text(
            r#"{"v":1,"type":"ping"}"#.into(),
        ))
        .await
        .expect("send ping");
    assert_eq!(
        next_control(&mut socket).await["type"],
        serde_json::json!("pong")
    );

    // Binary frames are PTY bytes.
    socket
        .send(tungstenite::Message::Binary(
            b"echo oab-pty-marker\n".to_vec().into(),
        ))
        .await
        .expect("send input");
    expect_output_containing(&mut socket, "oab-pty-marker").await;

    // Detach, then reconnect with a cursor: the replay is served from the ring
    // buffer under the attach lock, so the missed bytes arrive exactly once.
    socket
        .send(tungstenite::Message::Text(
            r#"{"v":1,"type":"detach"}"#.into(),
        ))
        .await
        .expect("send detach");
    let _ = wait_for_close(&mut socket).await;

    let url = format!("ws://{}/pty/kappa?since=0&rows=40&cols=132", harness.addr);
    let mut reattached = connect_url(
        &url,
        Some(("authorization", format!("Bearer {token}"))),
    )
    .await
    .connected();
    let notice = next_control(&mut reattached).await;
    assert_eq!(notice["type"], serde_json::json!("attach-notice"));
    assert!(
        notice["stream_offset"].as_u64().unwrap() > 0,
        "the cursor must be non-zero once the child has written"
    );
    expect_output_containing(&mut reattached, "oab-pty-marker").await;

    harness.shutdown().await;
}

#[tokio::test]
#[ignore = "binds a socket and spawns a PTY child"]
async fn malformed_control_frames_disconnect_as_a_policy_violation() {
    use futures_util::SinkExt;

    let harness = Harness::start(Duration::from_secs(60)).await;
    let token = harness.create("lambda").await;
    let mut socket = attach_with_header(harness.addr, "lambda", &token)
        .await
        .connected();
    assert_eq!(
        next_control(&mut socket).await["type"],
        serde_json::json!("attach-notice")
    );

    for bad in [
        r#"{"type":"ping"}"#,                            // no version
        r#"{"v":1,"type":"exec","cmd":"sh"}"#,           // unknown type
        r#"{"v":1,"type":"resize","rows":0,"cols":0}"#,  // out of bounds
    ] {
        socket
            .send(tungstenite::Message::Text(bad.into()))
            .await
            .expect("send malformed frame");
    }
    // 1008 policy violation: frame abuse is a protocol fault, deliberately not
    // one of the ADR's lifecycle close codes.
    assert_eq!(wait_for_close(&mut socket).await, 1008);

    harness.shutdown().await;
}

// ---------------------------------------------------------------------------
// Admin plane
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "binds a socket"]
async fn no_admin_operation_succeeds_without_the_credential() {
    let harness = Harness::start(Duration::from_secs(60)).await;

    let create_body = r#"{"name":"unauthorized","rows":24,"cols":80}"#;
    for (method, path, body) in [
        ("GET", "/admin/sessions", None),
        ("POST", "/admin/sessions", Some(create_body)),
        ("DELETE", "/admin/sessions/unauthorized", None),
        ("POST", "/admin/sessions/unauthorized/renew", None),
        ("POST", "/admin/sessions/unauthorized/restart", None),
    ] {
        let (status, _) = harness.request(method, path, None, body).await;
        assert_eq!(status, 401, "{method} {path} must require the credential");

        let wrong = "f".repeat(64);
        let (status, _) = harness.request(method, path, Some(&wrong), body).await;
        assert!(
            status == 401 || status == 429,
            "{method} {path} with a wrong credential returned {status}"
        );
    }

    // Nothing was created by any of the rejected attempts. The rejections armed
    // the admin throttle, so wait it out rather than racing it — the backoff
    // curve itself is unit tested in `admin_auth`.
    let mut listed = serde_json::Value::Null;
    let mut status = 0;
    for _ in 0..30 {
        let (code, body) = harness.admin("GET", "/admin/sessions", None).await;
        status = code;
        listed = body;
        if status == 200 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(status, 200, "admin list with the real credential: {listed}");
    assert_eq!(
        listed["sessions"].as_array().map(Vec::len),
        Some(0),
        "a rejected create must not have spawned anything"
    );
    // The guarantee is labelled wherever it is surfaced.
    assert!(listed["kill_domain"]["teardown_best_effort"].is_boolean());

    harness.shutdown().await;
}

#[tokio::test]
#[ignore = "binds a socket and spawns a PTY child"]
async fn the_attach_surface_has_no_minting_path() {
    let harness = Harness::start(Duration::from_secs(60)).await;
    let _token = harness.create("iota").await;

    // The admin credential is not an attach token: presenting it on the attach
    // surface authorizes nothing, and no route under /pty mints anything.
    assert_eq!(
        attach_with_header(harness.addr, "iota", &harness.admin_credential)
            .await
            .refused_status(),
        401
    );
    for path in ["/pty/iota/create", "/pty/iota/renew", "/pty"] {
        let (status, _) = harness
            .request("POST", path, Some(&harness.admin_credential), Some("{}"))
            .await;
        assert!(
            status == 404 || status == 405,
            "POST {path} must not exist on the attach surface (got {status})"
        );
    }

    harness.shutdown().await;
}

// ---------------------------------------------------------------------------
// `--validate-projection`
// ---------------------------------------------------------------------------

const GOOD_HASH: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn write_projection(name: &str, contents: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("openab-pty-{}-{name}.toml", std::process::id()));
    std::fs::write(&path, contents).expect("write projection");
    path
}

fn run_validate(path: &std::path::Path) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_openab-pty"))
        .arg("--validate-projection")
        .arg(path)
        .output()
        .expect("run openab-pty --validate-projection")
}

#[tokio::test]
#[ignore = "spawns the built binary"]
async fn validate_projection_accepts_a_delivered_projection() {
    let path = write_projection("good", &projection(test_command(), GOOD_HASH, 4));
    let output = run_validate(&path);
    assert!(
        output.status.success(),
        "a valid projection must be accepted: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
#[ignore = "spawns the built binary"]
async fn validate_projection_rejects_a_poisoned_projection() {
    let base = projection(test_command(), GOOD_HASH, 4);
    let cases = [
        // Both verifier-poisoning cases the ADR enumerates.
        (
            "secrets-interpolation",
            base.replace(GOOD_HASH, "${secrets.pty_admin_hash}"),
        ),
        (
            "malformed-hash",
            base.replace(GOOD_HASH, "sha256:not-a-verifier"),
        ),
        // Plus the surrounding class, so the guard is not narrower than the rule.
        (
            "secrets-table",
            format!("[secrets.refs]\nx = \"y\"\n{base}"),
        ),
        (
            "broker-section",
            format!("[discord]\nbot_token = \"x\"\n{base}"),
        ),
        (
            "cloud-reference",
            base.replace(test_command(), "aws-sm://openab/pty#command"),
        ),
        ("empty-hash", base.replace(GOOD_HASH, "")),
    ];
    for (name, contents) in cases {
        let path = write_projection(name, &contents);
        let output = run_validate(&path);
        assert!(
            !output.status.success(),
            "{name} must be rejected, and with a non-zero exit so CI can gate on it"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("REJECTED"),
            "{name} must say why it was rejected"
        );
        let _ = std::fs::remove_file(path);
    }
}

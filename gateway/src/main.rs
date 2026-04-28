mod adapters;
mod schema;

use anyhow::Result;
use axum::{
    extract::State,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use futures_util::{SinkExt, StreamExt};
use schema::GatewayReply;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tracing::{info, warn};

// --- App state (shared across all adapters) ---

pub struct AppState {
    /// Telegram bot token (None if Telegram disabled)
    pub telegram_bot_token: Option<String>,
    /// Telegram webhook secret token for request validation
    pub telegram_secret_token: Option<String>,
    /// LINE channel secret for signature validation
    pub line_channel_secret: Option<String>,
    /// LINE channel access token for reply API
    pub line_access_token: Option<String>,
    /// WebSocket authentication token
    pub ws_token: Option<String>,
    /// Teams adapter (None if Teams disabled)
    pub teams: Option<adapters::teams::TeamsAdapter>,
    /// service_url cache for Teams reply routing (conversation_id → (service_url, last_seen))
    pub teams_service_urls: Mutex<HashMap<String, (String, std::time::Instant)>>,
    /// Broadcast channel: gateway → OAB (events from all platforms)
    pub event_tx: broadcast::Sender<String>,
}

// --- WebSocket handler (OAB connects here) ---

async fn ws_handler(
    State(state): State<Arc<AppState>>,
    query: axum::extract::Query<HashMap<String, String>>,
    ws: axum::extract::WebSocketUpgrade,
) -> axum::response::Response {
    if let Some(ref expected) = state.ws_token {
        let provided = query.get("token").map(|s| s.as_str());
        if provided != Some(expected.as_str()) {
            warn!("WebSocket rejected: invalid or missing token");
            return axum::http::StatusCode::UNAUTHORIZED.into_response();
        }
    }
    ws.on_upgrade(move |socket| handle_oab_connection(state, socket))
}

async fn handle_oab_connection(state: Arc<AppState>, socket: axum::extract::ws::WebSocket) {
    use axum::extract::ws::Message;

    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut event_rx = state.event_tx.subscribe();

    info!("OAB client connected via WebSocket");

    // Forward gateway events → OAB
    let send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                Ok(event_json) = event_rx.recv() => {
                    if ws_tx.send(Message::Text(event_json.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // Receive OAB replies → route to correct platform
    let state_for_recv = state.clone();
    // Track per-message reaction state (Telegram replaces all reactions atomically)
    let reaction_state: Arc<Mutex<HashMap<String, Vec<String>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let recv_task = tokio::spawn(async move {
        let client = reqwest::Client::new();
        while let Some(Ok(msg)) = ws_rx.next().await {
            if let Message::Text(text) = msg {
                // Check if it's a response to a pending command
                if let Ok(resp) = serde_json::from_str::<schema::GatewayResponse>(&*text) {
                    if resp.schema == "openab.gateway.response.v1" {
                        let _ = state_for_recv.event_tx.send(text.to_string());
                        continue;
                    }
                }

                match serde_json::from_str::<GatewayReply>(&*text) {
                    Ok(reply) => {
                        info!(
                            platform = %reply.platform,
                            channel = %reply.channel.id,
                            command = ?reply.command.as_deref(),
                            "OAB → gateway reply"
                        );
                        match reply.platform.as_str() {
                        "telegram" => {
                            if let Some(ref token) = state_for_recv.telegram_bot_token {
                                adapters::telegram::handle_reply(
                                    &reply,
                                    token,
                                    &client,
                                    &state_for_recv.event_tx,
                                    &reaction_state,
                                )
                                .await;
                            } else {
                                warn!("reply for telegram but adapter not configured");
                            }
                        }
                        "line" => {
                            if let Some(ref token) = state_for_recv.line_access_token {
                                adapters::line::handle_reply(&reply, token, &client).await;
                            } else {
                                warn!("reply for line but adapter not configured");
                            }
                        }
                        "teams" => {
                            if let Some(ref teams) = state_for_recv.teams {
                                adapters::teams::handle_reply(
                                    &reply,
                                    teams,
                                    &state_for_recv.teams_service_urls,
                                )
                                .await;
                            } else {
                                warn!("reply for teams but adapter not configured");
                            }
                        }
                        other => warn!(platform = other, "unknown reply platform"),
                        }
                    }
                    Err(e) => warn!("invalid reply from OAB: {e}"),
                }
            }
        }
    });

    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }
    info!("OAB client disconnected");
}

async fn health() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let listen_addr = std::env::var("GATEWAY_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let ws_token = std::env::var("GATEWAY_WS_TOKEN").ok();

    if ws_token.is_none() {
        warn!("GATEWAY_WS_TOKEN not set — WebSocket connections are NOT authenticated (insecure)");
    }

    let (event_tx, _) = broadcast::channel::<String>(256);

    // --- Initialize adapters ---

    let mut app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health));

    // Telegram adapter
    let telegram_bot_token = std::env::var("TELEGRAM_BOT_TOKEN").ok();
    let telegram_secret_token = std::env::var("TELEGRAM_SECRET_TOKEN").ok();
    if telegram_bot_token.is_some() {
        let webhook_path =
            std::env::var("TELEGRAM_WEBHOOK_PATH").unwrap_or_else(|_| "/webhook/telegram".into());
        if telegram_secret_token.is_none() {
            warn!("TELEGRAM_SECRET_TOKEN not set — webhook requests are NOT validated (insecure)");
        }
        info!(path = %webhook_path, "telegram adapter enabled");
        app = app.route(&webhook_path, post(adapters::telegram::webhook));
    }

    // LINE adapter
    let line_channel_secret = std::env::var("LINE_CHANNEL_SECRET").ok();
    let line_access_token = std::env::var("LINE_CHANNEL_ACCESS_TOKEN").ok();
    if line_access_token.is_some() {
        info!("line adapter enabled");
        app = app.route("/webhook/line", post(adapters::line::webhook));
    }

    // Teams adapter
    let teams = adapters::teams::TeamsConfig::from_env().map(|config| {
        info!("teams adapter enabled");
        adapters::teams::TeamsAdapter::new(config)
    });
    if teams.is_some() {
        let webhook_path =
            std::env::var("TEAMS_WEBHOOK_PATH").unwrap_or_else(|_| "/webhook/teams".into());
        info!(path = %webhook_path, "teams webhook registered");
        app = app.route(&webhook_path, post(adapters::teams::webhook));
    }

    if telegram_bot_token.is_none() && line_access_token.is_none() && teams.is_none() {
        warn!("no adapters configured — set TELEGRAM_BOT_TOKEN, LINE_CHANNEL_ACCESS_TOKEN, and/or TEAMS_APP_ID + TEAMS_APP_SECRET");
    }

    let state = Arc::new(AppState {
        telegram_bot_token,
        telegram_secret_token,
        line_channel_secret,
        line_access_token,
        ws_token,
        teams,
        teams_service_urls: Mutex::new(HashMap::new()),
        event_tx,
    });

    let app = app.with_state(state.clone());

    // Periodic cleanup of stale Teams service_url entries (TTL: 4 hours)
    tokio::spawn(async move {
        let ttl = std::time::Duration::from_secs(4 * 3600);
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            let mut urls = state.teams_service_urls.lock().await;
            let before = urls.len();
            urls.retain(|_, (_, ts)| ts.elapsed() < ttl);
            let evicted = before - urls.len();
            if evicted > 0 {
                info!(
                    evicted,
                    remaining = urls.len(),
                    "teams service_url cache cleanup"
                );
            }
        }
    });

    info!(addr = %listen_addr, "gateway starting");
    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

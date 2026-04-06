use crate::acp::SessionPool;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::time::Instant;
use tracing::{error, info};

pub async fn serve(
    bind: String,
    pool: Arc<SessionPool>,
    started: Instant,
    discord_connected: Arc<AtomicBool>,
) {
    let listener = match TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            error!(bind = %bind, error = %e, "management server bind failed");
            return;
        }
    };
    info!(bind = %bind, "management server listening");

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, "management accept error");
                continue;
            }
        };
        let pool = pool.clone();
        let discord_connected = discord_connected.clone();
        tokio::spawn(async move {
            let (reader, mut writer) = stream.into_split();
            let mut buf_reader = BufReader::new(reader);
            let mut request_line = String::new();
            if buf_reader.read_line(&mut request_line).await.is_err() {
                return;
            }

            // Consume remaining headers
            let mut header = String::new();
            loop {
                header.clear();
                if buf_reader.read_line(&mut header).await.is_err() || header.trim().is_empty() {
                    break;
                }
            }

            let parts: Vec<&str> = request_line.trim().splitn(3, ' ').collect();
            if parts.len() < 2 {
                let _ = write_response(&mut writer, 400, &json!({"error": "bad request"})).await;
                return;
            }
            let method = parts[0];
            let path = parts[1];

            let (status, body) = match (method, path) {
                ("GET", "/healthz") => {
                    let uptime = started.elapsed().as_secs();
                    let connected = discord_connected.load(Ordering::Relaxed);
                    (200, json!({"status": "ok", "uptime_seconds": uptime, "discord_connected": connected}))
                }
                ("GET", "/sessions") => {
                    let sessions = pool.list_sessions().await;
                    let list: Vec<_> = sessions
                        .iter()
                        .map(|(id, alive, idle)| {
                            json!({"thread_id": id, "alive": alive, "idle_seconds": idle})
                        })
                        .collect();
                    (200, json!({"active_sessions": sessions.len(), "max_sessions": pool.max_sessions(), "sessions": list}))
                }
                ("DELETE", "/sessions") => {
                    let count = pool.remove_all_sessions().await;
                    (200, json!({"killed": count}))
                }
                ("DELETE", p) if p.starts_with("/sessions/") => {
                    let thread_id = &p["/sessions/".len()..];
                    if thread_id.is_empty() {
                        (400, json!({"error": "missing thread_id"}))
                    } else if pool.remove_session(thread_id).await {
                        (200, json!({"killed": thread_id}))
                    } else {
                        (404, json!({"error": "session not found"}))
                    }
                }
                _ => (404, json!({"error": "not found"})),
            };

            let _ = write_response(&mut writer, status, &body).await;
        });
    }
}

async fn write_response(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    status: u16,
    body: &serde_json::Value,
) -> std::io::Result<()> {
    let body = serde_json::to_string(body).unwrap_or_default();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    writer.write_all(resp.as_bytes()).await?;
    writer.flush().await
}

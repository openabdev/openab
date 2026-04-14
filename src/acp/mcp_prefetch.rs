use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

const DEFAULT_TIMEOUT_SECS: u64 = 10;
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const OPENAB_CLIENT_NAME: &str = "openab";
const OPENAB_CLIENT_VERSION: &str = "0.1.0";

pub async fn prefetch_mempalace_context(server: &Value) -> Result<Option<String>> {
    let status = fetch_mempalace_status(server).await?;
    Ok(build_startup_context(&status))
}

async fn fetch_mempalace_status(server: &Value) -> Result<Value> {
    match server.get("type").and_then(|value| value.as_str()) {
        Some("http") | Some("sse") => fetch_status_over_http(server).await,
        Some("stdio") | Some("local") => fetch_status_over_stdio(server).await,
        Some(other) => Err(anyhow!("unsupported mempalace MCP transport: {other}")),
        None if server.get("url").is_some() => fetch_status_over_http(server).await,
        None if server.get("command").is_some() => fetch_status_over_stdio(server).await,
        None => Err(anyhow!(
            "mempalace MCP server is missing both url and command"
        )),
    }
}

fn build_startup_context(status: &Value) -> Option<String> {
    let protocol = status.get("protocol").and_then(|value| value.as_str())?;
    let aaak = status.get("aaak_dialect").and_then(|value| value.as_str());
    let total_drawers = status.get("total_drawers").and_then(|value| value.as_u64());
    let wing_count = status
        .get("wings")
        .and_then(|value| value.as_object())
        .map(|wings| wings.len());

    let mut context = String::from(
        "MemPalace wake-up context was prefetched by the bridge from `mempalace_status`. \
This replaces any separate bootstrap prompt or wake-up tool call. Use this as the source \
of truth for memory behavior and diary writes instead of local CLAUDE.md or AGENTS.md files.",
    );

    if let Some(total_drawers) = total_drawers {
        context.push_str(&format!(
            "\n\nPalace overview: {total_drawers} drawer(s) loaded."
        ));
        if let Some(wing_count) = wing_count {
            context.push_str(&format!(" {wing_count} wing(s) are currently indexed."));
        }
    }

    context.push_str("\n\nProtocol:\n");
    context.push_str(protocol);

    if let Some(aaak) = aaak {
        context.push_str("\n\nAAAK Dialect:\n");
        context.push_str(aaak);
    }

    Some(context)
}

async fn fetch_status_over_http(server: &Value) -> Result<Value> {
    let url = server
        .get("url")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("mempalace HTTP MCP server is missing url"))?;
    let timeout_secs = request_timeout_secs(server);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .context("failed to build MCP HTTP client")?;

    let mut request = client
        .post(url)
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .header(reqwest::header::CONTENT_TYPE, "application/json");

    if let Some(headers) = server.get("headers").and_then(|value| value.as_object()) {
        for (name, value) in headers {
            if let Some(value) = value.as_str() {
                request = request.header(name, value);
            }
        }
    }

    let response = request
        .body(build_tools_call_request(1).to_string())
        .send()
        .await
        .with_context(|| format!("failed calling mempalace MCP at {url}"))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .context("failed reading mempalace MCP response body")?;
    if !status.is_success() {
        return Err(anyhow!(
            "mempalace MCP returned HTTP {}: {}",
            status,
            body.trim()
        ));
    }

    parse_http_tools_call_response(&body)
}

async fn fetch_status_over_stdio(server: &Value) -> Result<Value> {
    let command = server
        .get("command")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("mempalace stdio MCP server is missing command"))?;
    let args: Vec<String> = server
        .get("args")
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let timeout_secs = request_timeout_secs(server);

    let mut child = Command::new(command);
    child
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);

    if let Some(cwd) = server.get("cwd").and_then(|value| value.as_str()) {
        child.current_dir(cwd);
    }
    if let Some(env) = server.get("env").and_then(|value| value.as_object()) {
        for (key, value) in env {
            if let Some(value) = value.as_str() {
                child.env(key, expand_env(value));
            }
        }
    }

    let mut child = child
        .spawn()
        .with_context(|| format!("failed to spawn mempalace MCP command `{command}`"))?;
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("no MCP stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("no MCP stdout"))?;
    let mut reader = BufReader::new(stdout);

    send_stdio_request(&mut stdin, &build_initialize_request(1)).await?;
    read_stdio_response(&mut reader, 1, timeout_secs).await?;

    send_stdio_request(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await?;

    send_stdio_request(&mut stdin, &build_tools_call_request(2)).await?;
    let response = read_stdio_response(&mut reader, 2, timeout_secs).await?;
    let _ = child.kill().await;

    parse_tool_call_response(&response)
}

async fn send_stdio_request(stdin: &mut tokio::process::ChildStdin, payload: &Value) -> Result<()> {
    let line = serde_json::to_string(payload).context("failed to encode MCP stdio request")?;
    stdin
        .write_all(line.as_bytes())
        .await
        .context("failed writing MCP stdio request")?;
    stdin
        .write_all(b"\n")
        .await
        .context("failed terminating MCP stdio request")?;
    stdin.flush().await.context("failed flushing MCP stdio")?;
    Ok(())
}

async fn read_stdio_response(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    request_id: u64,
    timeout_secs: u64,
) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        let mut line = String::new();
        loop {
            line.clear();
            let read = reader
                .read_line(&mut line)
                .await
                .context("failed reading MCP stdio response")?;
            if read == 0 {
                return Err(anyhow!("mempalace MCP stdio closed before responding"));
            }
            let message: Value =
                serde_json::from_str(line.trim()).context("invalid MCP stdio JSON response")?;
            if message.get("id").and_then(|value| value.as_u64()) == Some(request_id) {
                return Ok(message);
            }
        }
    })
    .await
    .map_err(|_| anyhow!("timeout waiting for mempalace MCP stdio response"))?
}

fn build_initialize_request(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": OPENAB_CLIENT_NAME,
                "version": OPENAB_CLIENT_VERSION
            }
        }
    })
}

fn build_tools_call_request(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "name": "mempalace_status",
            "arguments": {}
        }
    })
}

fn parse_http_tools_call_response(body: &str) -> Result<Value> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("empty mempalace MCP HTTP response"));
    }

    if let Ok(response) = serde_json::from_str::<Value>(trimmed) {
        return parse_tool_call_response(&response);
    }

    for event_data in parse_sse_data_messages(trimmed) {
        if let Ok(response) = serde_json::from_str::<Value>(&event_data) {
            if response.get("id").is_some() || response.get("result").is_some() {
                return parse_tool_call_response(&response);
            }
        }
    }

    Err(anyhow!("unrecognized mempalace MCP HTTP response format"))
}

fn parse_sse_data_messages(body: &str) -> Vec<String> {
    let mut events = Vec::new();
    let mut data_lines = Vec::new();

    for line in body.lines() {
        if line.trim().is_empty() {
            if !data_lines.is_empty() {
                events.push(data_lines.join("\n"));
                data_lines.clear();
            }
            continue;
        }

        if let Some(data) = line.strip_prefix("data:") {
            let data = data.trim_start();
            if data != "[DONE]" {
                data_lines.push(data.to_string());
            }
        }
    }

    if !data_lines.is_empty() {
        events.push(data_lines.join("\n"));
    }

    events
}

fn parse_tool_call_response(response: &Value) -> Result<Value> {
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown MCP tool error");
        return Err(anyhow!("mempalace_status failed: {message}"));
    }

    let text = response
        .get("result")
        .and_then(|value| value.get("content"))
        .and_then(|value| value.as_array())
        .and_then(|content| {
            content.iter().find_map(|item| {
                item.get("text")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
            })
        })
        .ok_or_else(|| anyhow!("mempalace_status returned no text content"))?;

    serde_json::from_str(&text).context("invalid JSON payload inside mempalace_status response")
}

fn request_timeout_secs(server: &Value) -> u64 {
    let timeout = server
        .get("timeout")
        .and_then(|value| value.as_u64())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    timeout.clamp(1, 60)
}

fn expand_env(value: &str) -> String {
    if value.starts_with("${") && value.ends_with('}') {
        let key = &value[2..value.len() - 1];
        std::env::var(key).unwrap_or_default()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    #[test]
    fn builds_startup_context_from_status_payload() {
        let context = build_startup_context(&json!({
            "total_drawers": 12,
            "wings": {"project": 7, "people": 5},
            "protocol": "IMPORTANT — protocol",
            "aaak_dialect": "AAAK SPEC"
        }))
        .expect("context");

        assert!(context.contains("prefetched by the bridge"));
        assert!(context.contains("12 drawer(s)"));
        assert!(context.contains("IMPORTANT — protocol"));
        assert!(context.contains("AAAK SPEC"));
    }

    #[test]
    fn parses_sse_data_messages_into_tool_response() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"{\\n  \\\"protocol\\\": \\\"PROTO\\\"\\n}\"}]}}\n\n";
        let status = parse_http_tools_call_response(body).expect("parsed SSE response");

        assert_eq!(status["protocol"], "PROTO");
    }

    #[tokio::test]
    async fn prefetches_mempalace_context_over_http() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buffer = vec![0_u8; 4096];
            let mut request = Vec::new();

            loop {
                let read = stream.read(&mut buffer).await.expect("read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);

                let header_end = request
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|offset| offset + 4);
                let Some(header_end) = header_end else {
                    continue;
                };

                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let lower = line.to_ascii_lowercase();
                        lower
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);

                while request.len() < header_end + content_length {
                    let read = stream.read(&mut buffer).await.expect("read body");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                }

                let body = &request[header_end..header_end + content_length];
                let rpc: Value = serde_json::from_slice(body).expect("decode request JSON");
                assert_eq!(rpc["method"], "tools/call");
                assert_eq!(rpc["params"]["name"], "mempalace_status");

                let response_body = json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": "{\"protocol\":\"PROTO\",\"aaak_dialect\":\"SPEC\",\"total_drawers\":5,\"wings\":{\"project\":5}}"
                        }]
                    }
                })
                .to_string();

                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("write response");
                break;
            }
        });

        let context = prefetch_mempalace_context(&json!({
            "type": "http",
            "url": format!("http://{addr}/mcp")
        }))
        .await
        .expect("prefetch succeeded")
        .expect("startup context");

        assert!(context.contains("PROTO"));
        assert!(context.contains("SPEC"));
        server.await.expect("server task");
    }
}

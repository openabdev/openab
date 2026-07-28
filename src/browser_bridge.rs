//! `openab browser-bridge` — a stdio MCP server that is a thin relay to the per-pod browser
//! socket (Option C). The agent's MCP client spawns it per session; it reads
//! `OPENAB_BROWSER_CHANNEL` from its inherited env, wraps each stdin MCP request as
//! `{channel_id, request}`, forwards it to the core socket, and relays responses to stdout
//! verbatim. ALL browser MCP logic lives in core (`dispatch_browser_mcp`) — this is a pure pipe
//! + channel tag, so the config line agents carry is static (`{"command":"openab","args":
//! ["browser-bridge"]}`) and disambiguation is by the inherited env, never by config.
//!
//! stdout carries the MCP wire, so this path emits nothing to stdout except MCP responses
//! (diagnostics, if any, go to stderr).

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Wrap one stdin MCP request line into a socket frame `{channel_id, request}`. Returns `None`
/// for a blank/unparseable line (skip it) so a stray line can't break the relay.
fn wrap_frame(channel: &str, line: &str) -> Option<Vec<u8>> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let request: Value = serde_json::from_str(line).ok()?;
    let frame = json!({ "channel_id": channel, "request": request });
    let mut buf = serde_json::to_vec(&frame).ok()?;
    buf.push(b'\n');
    Some(buf)
}

/// Resolve this bridge's browser channel. The MCP client scrubs the child env (verified: cursor
/// launches us with only HOME/PATH/USER, or the pod env — never the per-session
/// OPENAB_BROWSER_CHANNEL openab injects into the AGENT), so we can't read it from our own env.
/// Instead walk UP the process tree and read OPENAB_BROWSER_CHANNEL from the first ancestor that
/// has it: the agent process openab spawned (channel injected via the pool) is always an ancestor
/// of this stdio MCP server, for EVERY vendor. Prefer our own env first (clients that DO pass it);
/// return "" if not found (core then reports "no browser attached").
fn resolve_channel() -> String {
    if let Ok(c) = std::env::var("OPENAB_BROWSER_CHANNEL") {
        if !c.is_empty() {
            return c;
        }
    }
    // Same ancestry walk the socket server now performs on our pid (review R2). Kept here only so
    // the frame still carries a channel for logging/compatibility — the server derives its own and
    // never trusts this one, so a wrong answer here is refused rather than honoured.
    openab_core::mcp_proxy::channel_from_process_ancestry(std::process::id()).unwrap_or_default()
}

/// Run the bridge: connect the core socket, then pump stdin→socket (channel-tagged) and
/// socket→stdout (verbatim MCP responses) until either side closes.
pub async fn run() -> std::io::Result<()> {
    let channel = resolve_channel();
    eprintln!("[openab browser-bridge] resolved channel={channel:?}");
    let sock =
        tokio::net::UnixStream::connect(openab_core::mcp_proxy::browser_socket_path()).await?;
    let (sock_rd, sock_wr) = sock.into_split();
    pump(
        channel,
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        BufReader::new(sock_rd),
        sock_wr,
    )
    .await
}

/// The relay, generic over the four streams so it can be tested with in-memory pipes. Ends when
/// either stdin (agent gone) or the socket (core gone) closes.
async fn pump<In, Out, SockR, SockW>(
    channel: String,
    mut stdin: In,
    mut stdout: Out,
    mut sock_rd: SockR,
    mut sock_wr: SockW,
) -> std::io::Result<()>
where
    In: AsyncBufReadExt + Unpin,
    Out: AsyncWriteExt + Unpin,
    SockR: AsyncBufReadExt + Unpin,
    SockW: AsyncWriteExt + Unpin,
{
    let to_sock = async {
        let mut line = String::new();
        loop {
            line.clear();
            if stdin.read_line(&mut line).await? == 0 {
                break; // stdin closed → agent gone
            }
            if let Some(frame) = wrap_frame(&channel, &line) {
                sock_wr.write_all(&frame).await?;
                sock_wr.flush().await?;
            }
        }
        Ok::<(), std::io::Error>(())
    };
    let to_stdout = async {
        let mut line = String::new();
        loop {
            line.clear();
            if sock_rd.read_line(&mut line).await? == 0 {
                break; // socket closed → core gone
            }
            stdout.write_all(line.as_bytes()).await?;
            stdout.flush().await?;
        }
        Ok::<(), std::io::Error>(())
    };
    // Whichever side closes first ends the relay; the other pump is dropped (we're shutting down).
    tokio::select! {
        r = to_sock => r,
        r = to_stdout => r,
    }
}

#[cfg(test)]
mod tests {
    use super::{pump, wrap_frame};

    // The `/proc` parsing these covered now lives in openab-core, next to the socket server that
    // authenticates with it, and is tested there.

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[test]
    fn wrap_frame_tags_the_channel_and_appends_newline() {
        let out = wrap_frame("acp_win1", r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        assert_eq!(*out.last().unwrap(), b'\n');
        let v: serde_json::Value = serde_json::from_slice(&out[..out.len() - 1]).unwrap();
        assert_eq!(v["channel_id"], "acp_win1");
        assert_eq!(v["request"]["method"], "tools/list");
        assert_eq!(v["request"]["id"], 1);
    }

    #[test]
    fn wrap_frame_skips_blank_and_malformed_lines() {
        assert!(wrap_frame("c", "   ").is_none());
        assert!(wrap_frame("c", "").is_none());
        assert!(wrap_frame("c", "not json").is_none());
    }

    #[tokio::test]
    async fn pump_relays_request_to_socket_and_response_to_stdout() {
        // Four in-memory pipes standing in for stdin, stdout, and the two socket halves.
        let (mut stdin_w, stdin_r) = tokio::io::duplex(1024);
        let (stdout_w, mut stdout_r) = tokio::io::duplex(1024);
        let (mut sock_peer_w, sock_rd) = tokio::io::duplex(1024); // core → bridge (responses)
        let (sock_wr, mut sock_peer_r) = tokio::io::duplex(1024); // bridge → core (frames)

        let handle = tokio::spawn(pump(
            "acp_win1".to_string(),
            BufReader::new(stdin_r),
            stdout_w,
            BufReader::new(sock_rd),
            sock_wr,
        ));

        // Agent writes an MCP request on stdin → bridge should emit a channel-tagged frame to core.
        stdin_w
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"tools/call\",\"params\":{}}\n")
            .await
            .unwrap();
        let mut frame = String::new();
        BufReader::new(&mut sock_peer_r).read_line(&mut frame).await.unwrap();
        let fv: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(fv["channel_id"], "acp_win1");
        assert_eq!(fv["request"]["id"], 9);

        // Core writes an MCP response on the socket → bridge should relay it verbatim to stdout.
        sock_peer_w
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{\"ok\":true}}\n")
            .await
            .unwrap();
        let mut out = String::new();
        BufReader::new(&mut stdout_r).read_line(&mut out).await.unwrap();
        let ov: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(ov["id"], 9);
        assert_eq!(ov["result"]["ok"], true);

        drop(stdin_w); // agent gone → relay ends
        let _ = handle.await;
    }
}

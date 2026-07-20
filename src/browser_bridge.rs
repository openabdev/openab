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
    let mut pid = std::process::id();
    for _ in 0..16 {
        if let Ok(bytes) = std::fs::read(format!("/proc/{pid}/environ")) {
            if let Some(c) = parse_channel_from_environ(&bytes) {
                return c;
            }
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            break;
        };
        match parse_ppid_from_stat(&stat) {
            Some(ppid) if ppid > 1 => pid = ppid, // step up (stop at init/tini = 1)
            _ => break,
        }
    }
    String::new()
}

/// Extract OPENAB_BROWSER_CHANNEL from a null-separated `/proc/<pid>/environ` blob.
fn parse_channel_from_environ(bytes: &[u8]) -> Option<String> {
    for kv in bytes.split(|&b| b == 0) {
        if let Some(rest) = kv.strip_prefix(b"OPENAB_BROWSER_CHANNEL=") {
            if !rest.is_empty() {
                return Some(String::from_utf8_lossy(rest).into_owned());
            }
        }
    }
    None
}

/// Parse the parent PID from a `/proc/<pid>/stat` line. Field 2 (`comm`) is parenthesized and may
/// contain spaces or `)`, so split after the LAST `)`: the remainder is "state ppid pgrp ...".
fn parse_ppid_from_stat(stat: &str) -> Option<u32> {
    let after = stat.rsplit_once(')')?.1;
    after.split_whitespace().nth(1)?.parse().ok()
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
    use super::{parse_channel_from_environ, parse_ppid_from_stat, pump, wrap_frame};

    #[test]
    fn parse_ppid_handles_comm_with_spaces_and_parens() {
        assert_eq!(parse_ppid_from_stat("834 (sh) S 25 834 25 0 -1 ..."), Some(25));
        assert_eq!(parse_ppid_from_stat("658 (cursor agent) R 25 658 ..."), Some(25));
        assert_eq!(parse_ppid_from_stat("5 (weird )proc) S 3 5 ..."), Some(3)); // ')' inside comm
        assert_eq!(parse_ppid_from_stat("nonsense"), None);
    }

    #[test]
    fn parse_channel_from_environ_finds_the_var() {
        let env = b"HOME=/home/agent\0OPENAB_BROWSER_CHANNEL=acp_xyz\0PATH=/bin\0";
        assert_eq!(parse_channel_from_environ(env).as_deref(), Some("acp_xyz"));
        assert_eq!(parse_channel_from_environ(b"HOME=/x\0PATH=/y\0"), None);
        assert_eq!(parse_channel_from_environ(b"OPENAB_BROWSER_CHANNEL=\0"), None); // empty ignored
    }
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

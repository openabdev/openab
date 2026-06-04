/// Format any error for user display in Discord.
///
/// Handles two error categories:
/// - **Coded errors** (code != 0): JSON-RPC or HTTP status codes from upstream agent.
/// - **Startup/connection errors** (code == 0): Errors from pool.rs or connection.rs
///   where only the message string is available.
///
/// Provider-agnostic: no provider-specific strings, message text passed through verbatim.
pub fn format_user_error(message: &str) -> String {
    let msg_lower = message.to_lowercase();

    // Startup / connection errors (code == 0 from anyhow)
    if msg_lower.contains("timeout waiting for") {
        // Use msg_lower for extraction to stay case-insistent with the match above.
        // msg_lower and message are the same length, so byte offsets are valid.
        if let Some(start) = msg_lower.find("timeout waiting for ") {
            let rest = &message[start + "timeout waiting for ".len()..];
            let method = rest.split_whitespace().next().unwrap_or("request");
            return format!(
                "**Request Timeout**\nTimeout waiting for {}, please try again.",
                method
            );
        }
        return "**Request Timeout**\nTimeout waiting for a response, please try again."
            .to_string();
    }
    if msg_lower.contains("connection closed") || msg_lower.contains("channel closed") {
        return "**Connection Lost**\nThe connection to the agent was lost, please try again."
            .to_string();
    }
    if msg_lower.contains("failed to spawn") || msg_lower.contains("no such file") {
        return "**Agent Not Found**\nCould not start the agent — please check your configuration."
            .to_string();
    }
    if msg_lower.contains("pool exhausted") {
        return "**Service Busy**\nAll agent sessions are in use, please try again shortly."
            .to_string();
    }
    if msg_lower.contains("invalid api key") || msg_lower.contains("unauthorized") {
        return "**Unauthorized**\nPlease check your API key configuration.".to_string();
    }

    // Unknown error — pass through as-is
    if message.is_empty() {
        "**Error**\nAn unknown error occurred.".to_string()
    } else {
        format!("**Error**\n{}", message)
    }
}

/// Format coded error from ACP agent for display in Discord.
/// Used for response errors that have a JSON-RPC or HTTP status code.
///
/// `data_message` is the optional detail extracted from `error.data.message`
/// (see PR #885).
///
/// `stderr_tail` is the agent's recent stderr output, used as a fallback
/// detail when `data_message` is absent. Some agents (e.g. opencode in
/// #25568, hermes-agent) return `-32603` with `data: {}` or no `data` at
/// all, leaving only stderr to carry the real cause. See #1000, #998.
///
/// Rendering rules:
/// 1. If `data_message` is present and non-empty, render it (existing path).
/// 2. Else if `stderr_tail` is non-empty, render it as a "Recent agent
///    output:" blockquote (last 5 lines, to keep Discord message compact).
/// 3. Else, plain `prefix (code: N)`.
/// 4. `data_message` and `stderr_tail` are never shown together — `data`
///    is the structured channel and should not be noise-merged with stderr.
///
/// Public for reuse by other adapters (e.g. Slack).
pub fn format_coded_error(
    code: i64,
    message: &str,
    data_message: Option<&str>,
    stderr_tail: &[String],
) -> String {
    let prefix = match code {
        400 => "**Bad Request**",
        401 => "**Unauthorized**",
        403 => "**Forbidden**",
        404 => "**Not Found**",
        408 => "**Request Timeout**",
        429 => "**Rate Limited**",
        500 => "**Internal Server Error**",
        502 => "**Bad Gateway**",
        503 => "**Service Unavailable**",
        504 => "**Gateway Timeout**",
        -32600 => "**Invalid Request**",
        -32601 => "**Method Not Found**",
        -32602 => "**Invalid Params**",
        -32603 => "**Internal Error**",
        -32099..=-32000 => "**Server Error**",
        _ => "**Error**",
    };
    let mut out = if message.is_empty() {
        format!("{} (code: {})", prefix, code)
    } else {
        format!("{} (code: {})\n{}", prefix, code, message)
    };
    // `Option::filter` collapses `Some("")` to `None` so the stderr fallback
    // fires when the agent populates an empty `data.message`. Without filter,
    // `if let Some(_)` matches `Some("")` and the inner check is a no-op,
    // stranding the user with no diagnostic. Regression test:
    // `format_coded_error_empty_data_string_falls_through_to_stderr`.
    if let Some(detail) = data_message.filter(|s| !s.is_empty()) {
        if !message.contains(detail) {
            out.push_str("\n> ");
            out.push_str(detail);
        }
    } else if !stderr_tail.is_empty() {
        // Fallback: opaque error envelope, no `data` to extract.
        // Show the last few lines so users get actionable signal without
        // flooding the channel. Older lines are still in the operator's
        // `kubectl logs` via the existing tracing::warn! path.
        out.push_str("\n> _Recent agent output:_\n");
        // Cap at 5 lines regardless of buffer size — Discord message budget
        // and human attention budget both bounded.
        let start = stderr_tail.len().saturating_sub(5);
        for line in &stderr_tail[start..] {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── format_user_error tests ─────────────────────────────────────────────

    #[test]
    fn format_user_error_timeout() {
        let result = format_user_error("timeout waiting for session/new response");
        assert!(result.contains("Request Timeout"));
        assert!(result.contains("session/new"));
    }

    #[test]
    fn format_user_error_connection_closed() {
        let result = format_user_error("connection closed");
        assert!(result.contains("Connection Lost"));
    }

    #[test]
    fn format_user_error_channel_closed() {
        let result = format_user_error("channel closed");
        assert!(result.contains("Connection Lost"));
    }

    #[test]
    fn format_user_error_failed_to_spawn() {
        let result = format_user_error("failed to spawn /some/path: No such file");
        assert!(result.contains("Agent Not Found"));
        assert!(result.contains("the agent")); // generic, no provider name
    }

    #[test]
    fn format_user_error_no_such_file() {
        let result = format_user_error("binary /usr/bin/nonexistent: no such file");
        assert!(result.contains("Agent Not Found"));
    }

    #[test]
    fn format_user_error_pool_exhausted() {
        let result = format_user_error("pool exhausted (5 sessions)");
        assert!(result.contains("Service Busy"));
    }

    #[test]
    fn format_user_error_invalid_api_key() {
        let result = format_user_error("invalid api key");
        assert!(result.contains("Unauthorized"));
    }

    #[test]
    fn format_user_error_unauthorized() {
        let result = format_user_error("unauthorized: token rejected");
        assert!(result.contains("Unauthorized"));
    }

    #[test]
    fn format_user_error_unknown() {
        let result = format_user_error("something went wrong");
        assert!(result.contains("Error"));
        assert!(result.contains("something went wrong"));
    }

    #[test]
    fn format_user_error_empty() {
        let result = format_user_error("");
        assert!(result.contains("Error"));
        assert!(result.contains("unknown"));
    }

    #[test]
    fn format_user_error_case_insensitive() {
        assert!(format_user_error("TIMEOUT WAITING FOR foo").contains("Timeout"));
        assert!(format_user_error("CONNECTION CLOSED").contains("Connection"));
        assert!(format_user_error("POOL EXHAUSTED").contains("Busy"));
    }

    #[test]
    fn format_user_error_mixed_case_timeout() {
        // Case-insensitive matching should still extract method correctly
        let result = format_user_error("Timeout Waiting For custom/method");
        assert!(result.contains("Request Timeout"));
        assert!(result.contains("custom/method"));
    }

    // ─── format_coded_error tests ───────────────────────────────────────────

    fn empty_tail() -> Vec<String> {
        Vec::new()
    }

    #[test]
    fn format_coded_error_401() {
        let result = format_coded_error(401, "invalid token", None, &empty_tail());
        assert!(result.contains("Unauthorized"));
        assert!(result.contains("401"));
        assert!(result.contains("invalid token"));
    }

    #[test]
    fn format_coded_error_429() {
        let result = format_coded_error(429, "", None, &empty_tail());
        assert!(result.contains("Rate Limited"));
        assert!(result.contains("429"));
        assert!(!result.contains("\n")); // no message, no newline
    }

    #[test]
    fn format_coded_error_503() {
        let result = format_coded_error(503, "service unavailable", None, &empty_tail());
        assert!(result.contains("Service Unavailable"));
        assert!(result.contains("503"));
        assert!(result.contains("service unavailable"));
    }

    #[test]
    fn format_coded_error_json_rpc() {
        let result = format_coded_error(-32602, "missing required parameter", None, &empty_tail());
        assert!(result.contains("Invalid Params"));
        assert!(result.contains("-32602"));
    }

    #[test]
    fn format_coded_error_server_error_range() {
        let result = format_coded_error(-32050, "internal failure", None, &empty_tail());
        assert!(result.contains("Server Error"));
        assert!(result.contains("-32050"));
    }

    #[test]
    fn format_coded_error_connection_error() {
        let result = format_coded_error(-32000, "connection refused", None, &empty_tail());
        assert!(result.contains("Server Error")); // -32000 falls in -32099..=-32000 range
        assert!(result.contains("-32000"));
    }

    #[test]
    fn format_coded_error_unknown_code() {
        let result = format_coded_error(999, "something happened", None, &empty_tail());
        assert!(result.contains("Error"));
        assert!(result.contains("999"));
        assert!(result.contains("something happened"));
    }

    #[test]
    fn format_coded_error_with_data_message() {
        let result = format_coded_error(-32603, "Internal error", Some("model not supported"), &empty_tail());
        assert!(result.contains("Internal Error"));
        assert!(result.contains("model not supported"));
    }

    #[test]
    fn format_coded_error_data_message_not_duplicated() {
        // If data_message is already in message, don't repeat it
        let result = format_coded_error(-32603, "model not supported", Some("model not supported"), &empty_tail());
        assert_eq!(result.matches("model not supported").count(), 1);
    }

    // ─── stderr_tail fallback tests (issue #1000 / #998) ────────────────────

    /// The headline case: opencode returns `data: {}` with a real auth error
    /// in stderr. Before this fix the user saw "Internal error" with no hint.
    /// After: they see the actual cause in the blockquote.
    #[test]
    fn format_coded_error_stderr_tail_when_data_empty() {
        let stderr: Vec<String> = vec![
            "Error: ANTHROPIC_API_KEY not set".to_string(),
        ];
        let result = format_coded_error(-32603, "Internal error", None, &stderr);
        assert!(result.contains("Internal Error"));
        assert!(result.contains("-32603"));
        // stderr tail surfaces because data_message is None
        assert!(result.contains("Recent agent output"));
        assert!(result.contains("ANTHROPIC_API_KEY"));
    }

    /// When `data.message` is present (codex-acp / hermes-with-data path),
    /// stderr must NOT be appended to avoid noisy duplicates. #1000.
    #[test]
    fn format_coded_error_data_message_suppresses_stderr() {
        let stderr: Vec<String> = vec![
            "DEBUG: connection pool idle".to_string(),
        ];
        let result = format_coded_error(
            -32603,
            "Internal error",
            Some("model not supported"),
            &stderr,
        );
        // data_message path takes precedence
        assert!(result.contains("model not supported"));
        assert!(!result.contains("Recent agent output"));
        assert!(!result.contains("connection pool idle"));
    }

    /// Cap at 5 lines even if the ring buffer holds 50. Discord message
    /// budget is bounded; older lines still available in kubectl logs.
    #[test]
    fn format_coded_error_stderr_tail_caps_at_five_lines() {
        let stderr: Vec<String> = (1..=20)
            .map(|i| format!("line {i}"))
            .collect();
        let result = format_coded_error(-32603, "Internal error", None, &stderr);
        // Last 5 lines (16..20) shown, earlier not
        assert!(result.contains("line 16"));
        assert!(result.contains("line 20"));
        assert!(!result.contains("line 15"));
    }

    /// Empty stderr + empty data → fallback to plain error (no panic, no junk).
    #[test]
    fn format_coded_error_no_data_no_stderr() {
        let result = format_coded_error(-32603, "Internal error", None, &empty_tail());
        assert!(result.contains("Internal Error"));
        assert!(!result.contains("Recent agent output"));
    }

    /// Empty data_message (Some("")) should be treated like None so the
    /// stderr fallback still fires — some agents may emit `data: {message: ""}`.
    #[test]
    fn format_coded_error_empty_data_string_falls_through_to_stderr() {
        let stderr: Vec<String> = vec!["real error in stderr".to_string()];
        let result = format_coded_error(-32603, "Internal error", Some(""), &stderr);
        assert!(result.contains("Recent agent output"));
        assert!(result.contains("real error in stderr"));
    }
}

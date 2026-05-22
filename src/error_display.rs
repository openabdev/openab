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
/// Public for reuse by other adapters (e.g. Slack).
///
/// When the upstream JSON-RPC error carries `data.details` (codex-acp /
/// acpx convention), the detail string is appended so users can tell
/// apart causes that otherwise share a single code+label (e.g. -32603
/// "Internal Error" for model deprecation vs. auth vs. peer-dep failure).
pub fn format_coded_error(err: &crate::acp::protocol::JsonRpcError) -> String {
    let prefix = match err.code {
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
    let head = format!("{} (code: {})", prefix, err.code);
    let details = err.data_details().unwrap_or("").trim();

    let body = match (err.message.as_str(), details) {
        ("", "") => return head,
        ("", d) => d.to_string(),
        (m, "") => m.to_string(),
        // Avoid duplicate text when the adapter already echoed `details`
        // into `message` (some agents do this for legacy clients).
        (m, d) if m.contains(d) => m.to_string(),
        (m, d) => format!("{}\n{}", m, d),
    };
    format!("{}\n{}", head, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── format_user_error tests ─────────────────────────────────────────────

    #[test]
    fn test_format_user_error_timeout() {
        let result = format_user_error("timeout waiting for session/new response");
        assert!(result.contains("Request Timeout"));
        assert!(result.contains("session/new"));
    }

    #[test]
    fn test_format_user_error_connection_closed() {
        let result = format_user_error("connection closed");
        assert!(result.contains("Connection Lost"));
    }

    #[test]
    fn test_format_user_error_channel_closed() {
        let result = format_user_error("channel closed");
        assert!(result.contains("Connection Lost"));
    }

    #[test]
    fn test_format_user_error_failed_to_spawn() {
        let result = format_user_error("failed to spawn /some/path: No such file");
        assert!(result.contains("Agent Not Found"));
        assert!(result.contains("the agent")); // generic, no provider name
    }

    #[test]
    fn test_format_user_error_no_such_file() {
        let result = format_user_error("binary /usr/bin/nonexistent: no such file");
        assert!(result.contains("Agent Not Found"));
    }

    #[test]
    fn test_format_user_error_pool_exhausted() {
        let result = format_user_error("pool exhausted (5 sessions)");
        assert!(result.contains("Service Busy"));
    }

    #[test]
    fn test_format_user_error_invalid_api_key() {
        let result = format_user_error("invalid api key");
        assert!(result.contains("Unauthorized"));
    }

    #[test]
    fn test_format_user_error_unauthorized() {
        let result = format_user_error("unauthorized: token rejected");
        assert!(result.contains("Unauthorized"));
    }

    #[test]
    fn test_format_user_error_unknown() {
        let result = format_user_error("something went wrong");
        assert!(result.contains("Error"));
        assert!(result.contains("something went wrong"));
    }

    #[test]
    fn test_format_user_error_empty() {
        let result = format_user_error("");
        assert!(result.contains("Error"));
        assert!(result.contains("unknown"));
    }

    #[test]
    fn test_format_user_error_case_insensitive() {
        assert!(format_user_error("TIMEOUT WAITING FOR foo").contains("Timeout"));
        assert!(format_user_error("CONNECTION CLOSED").contains("Connection"));
        assert!(format_user_error("POOL EXHAUSTED").contains("Busy"));
    }

    #[test]
    fn test_format_user_error_mixed_case_timeout() {
        // Case-insensitive matching should still extract method correctly
        let result = format_user_error("Timeout Waiting For custom/method");
        assert!(result.contains("Request Timeout"));
        assert!(result.contains("custom/method"));
    }

    // ─── format_coded_error tests ───────────────────────────────────────────

    use crate::acp::protocol::JsonRpcError;
    use serde_json::json;

    fn err(code: i64, message: &str) -> JsonRpcError {
        JsonRpcError {
            code,
            message: message.into(),
            data: None,
        }
    }

    fn err_with_data(code: i64, message: &str, data: serde_json::Value) -> JsonRpcError {
        JsonRpcError {
            code,
            message: message.into(),
            data: Some(data),
        }
    }

    #[test]
    fn test_format_coded_error_401() {
        let result = format_coded_error(&err(401, "invalid token"));
        assert!(result.contains("Unauthorized"));
        assert!(result.contains("401"));
        assert!(result.contains("invalid token"));
    }

    #[test]
    fn test_format_coded_error_429() {
        let result = format_coded_error(&err(429, ""));
        assert!(result.contains("Rate Limited"));
        assert!(result.contains("429"));
        assert!(!result.contains("\n")); // no message, no newline
    }

    #[test]
    fn test_format_coded_error_503() {
        let result = format_coded_error(&err(503, "service unavailable"));
        assert!(result.contains("Service Unavailable"));
        assert!(result.contains("503"));
        assert!(result.contains("service unavailable"));
    }

    #[test]
    fn test_format_coded_error_json_rpc() {
        let result = format_coded_error(&err(-32602, "missing required parameter"));
        assert!(result.contains("Invalid Params"));
        assert!(result.contains("-32602"));
    }

    #[test]
    fn test_format_coded_error_server_error_range() {
        let result = format_coded_error(&err(-32050, "internal failure"));
        assert!(result.contains("Server Error"));
        assert!(result.contains("-32050"));
    }

    #[test]
    fn test_format_coded_error_connection_error() {
        let result = format_coded_error(&err(-32000, "connection refused"));
        assert!(result.contains("Server Error")); // -32000 falls in -32099..=-32000 range
        assert!(result.contains("-32000"));
    }

    #[test]
    fn test_format_coded_error_unknown_code() {
        let result = format_coded_error(&err(999, "something happened"));
        assert!(result.contains("Error"));
        assert!(result.contains("999"));
        assert!(result.contains("something happened"));
    }

    // ─── data.details surfacing (new behavior) ─────────────────────────────

    #[test]
    fn test_format_coded_error_appends_data_details() {
        // codex-acp model deprecation: message is generic, details has model id
        let result = format_coded_error(&err_with_data(
            -32603,
            "Internal error",
            json!({"details": "model 'gpt-5.2-codex' is no longer supported"}),
        ));
        assert!(result.contains("Internal Error"));
        assert!(result.contains("-32603"));
        assert!(result.contains("Internal error"));
        assert!(result.contains("gpt-5.2-codex"));
    }

    #[test]
    fn test_format_coded_error_details_only_when_message_empty() {
        let result = format_coded_error(&err_with_data(
            -32603,
            "",
            json!({"details": "query closed before response received"}),
        ));
        assert!(result.contains("Internal Error"));
        assert!(result.contains("query closed before response received"));
    }

    #[test]
    fn test_format_coded_error_no_duplicate_when_message_contains_details() {
        // Some adapters echo details into message for legacy clients
        let result = format_coded_error(&err_with_data(
            -32603,
            "Internal error: session timed out",
            json!({"details": "session timed out"}),
        ));
        assert_eq!(result.matches("session timed out").count(), 1);
    }

    #[test]
    fn test_format_coded_error_ignores_non_string_details() {
        let result = format_coded_error(&err_with_data(
            -32603,
            "Internal error",
            json!({"details": {"nested": "object"}}),
        ));
        assert!(result.contains("Internal error"));
        assert!(!result.contains("nested"));
        assert!(!result.contains("object"));
    }

    #[test]
    fn test_format_coded_error_ignores_unknown_data_shape() {
        let result = format_coded_error(&err_with_data(
            -32603,
            "Internal error",
            json!({"unrelated": "field"}),
        ));
        assert!(result.contains("Internal error"));
        assert!(!result.contains("unrelated"));
    }

    #[test]
    fn test_format_coded_error_handles_empty_details_string() {
        let result = format_coded_error(&err_with_data(
            -32603,
            "Internal error",
            json!({"details": "   "}),
        ));
        // Whitespace-only details must not produce a trailing blank line.
        assert!(!result.ends_with('\n'));
        assert!(result.contains("Internal error"));
    }
}

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
            return format!("**Request Timeout**\nTimeout waiting for {}, please try again.", method);
        }
        return "**Request Timeout**\nTimeout waiting for a response, please try again.".to_string();
    }
    if msg_lower.contains("connection closed") || msg_lower.contains("channel closed") {
        return "**Connection Lost**\nThe connection to the agent was lost, please try again.".to_string();
    }
    if msg_lower.contains("failed to spawn") || msg_lower.contains("no such file") {
        return "**Agent Not Found**\nCould not start the agent — please check your configuration.".to_string();
    }
    if msg_lower.contains("pool exhausted") {
        return "**Service Busy**\nAll agent sessions are in use, please try again shortly.".to_string();
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
pub fn format_coded_error(code: i64, message: &str) -> String {
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
    if message.is_empty() {
        format!("{} (code: {})", prefix, code)
    } else {
        format!("{} (code: {})\n{}", prefix, code, message)
    }
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

    #[test]
    fn test_format_coded_error_401() {
        let result = format_coded_error(401, "invalid token");
        assert!(result.contains("Unauthorized"));
        assert!(result.contains("401"));
        assert!(result.contains("invalid token"));
    }

    #[test]
    fn test_format_coded_error_429() {
        let result = format_coded_error(429, "");
        assert!(result.contains("Rate Limited"));
        assert!(result.contains("429"));
        assert!(!result.contains("\n")); // no message, no newline
    }

    #[test]
    fn test_format_coded_error_503() {
        let result = format_coded_error(503, "service unavailable");
        assert!(result.contains("Service Unavailable"));
        assert!(result.contains("503"));
        assert!(result.contains("service unavailable"));
    }

    #[test]
    fn test_format_coded_error_json_rpc() {
        let result = format_coded_error(-32602, "missing required parameter");
        assert!(result.contains("Invalid Params"));
        assert!(result.contains("-32602"));
    }

    #[test]
    fn test_format_coded_error_server_error_range() {
        let result = format_coded_error(-32050, "internal failure");
        assert!(result.contains("Server Error"));
        assert!(result.contains("-32050"));
    }

    #[test]
    fn test_format_coded_error_connection_error() {
        let result = format_coded_error(-32000, "connection refused");
        assert!(result.contains("Server Error")); // -32000 falls in -32099..=-32000 range
        assert!(result.contains("-32000"));
    }

    #[test]
    fn test_format_coded_error_unknown_code() {
        let result = format_coded_error(999, "something happened");
        assert!(result.contains("Error"));
        assert!(result.contains("999"));
        assert!(result.contains("something happened"));
    }
}

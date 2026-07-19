//! Core-hosted MCP proxy server for MCP-over-ACP browser control (feature `acp-mcp`).
//!
//! Per ADR §7 (D3), OpenAB **core** hosts an in-process Streamable-HTTP MCP server on
//! loopback that the colocated agent CLI connects to as a normal MCP client. The server is a
//! proxy: its tool list + tool execution are backed by the remote browser extension over the
//! `/acp` MCP-over-ACP tunnel (wired in T5.3). Per D4 the browser tool set is
//! **static-advertised** regardless of whether an extension is currently attached — a call
//! while disconnected returns a "browser not connected" error rather than hiding the tools.
//!
//! This module currently provides the static tool set; the `ServerHandler` + loopback
//! listener (`spawn_mcp_server`) and the tunnel wiring land in the following T5 sub-ticks.

use rmcp::model::{object, Tool};
use serde_json::json;

/// The fixed set of browser tools OpenAB advertises over MCP (D4 static-advertise). DOM-
/// semantic actions the extension executes in the user's active tab; model-agnostic.
/// Consumed by the `ServerHandler::list_tools` impl wired in the next T5 sub-tick.
#[allow(dead_code)]
pub(crate) fn browser_tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "browser.click",
            "Click the element matching a CSS selector in the active browser tab.",
            object(json!({
                "type": "object",
                "properties": { "selector": { "type": "string", "description": "CSS selector" } },
                "required": ["selector"]
            })),
        ),
        Tool::new(
            "browser.read_dom",
            "Read a snapshot of the active tab's DOM (optionally scoped to a selector).",
            object(json!({
                "type": "object",
                "properties": { "selector": { "type": "string", "description": "optional CSS selector to scope the snapshot" } }
            })),
        ),
        Tool::new(
            "browser.navigate",
            "Navigate the active browser tab to a URL.",
            object(json!({
                "type": "object",
                "properties": { "url": { "type": "string", "description": "absolute URL" } },
                "required": ["url"]
            })),
        ),
        Tool::new(
            "browser.type",
            "Type text into the element matching a CSS selector in the active tab.",
            object(json!({
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "CSS selector" },
                    "text": { "type": "string", "description": "text to type" }
                },
                "required": ["selector", "text"]
            })),
        ),
        Tool::new(
            "browser.screenshot",
            "Capture a screenshot of the active browser tab.",
            object(json!({ "type": "object", "properties": {} })),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::browser_tools;

    #[test]
    fn browser_tools_advertises_the_fixed_set() {
        let tools = browser_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(
            names,
            [
                "browser.click",
                "browser.read_dom",
                "browser.navigate",
                "browser.type",
                "browser.screenshot"
            ]
        );
    }

    #[test]
    fn every_browser_tool_has_an_object_input_schema() {
        for t in browser_tools() {
            assert_eq!(
                t.input_schema.get("type").and_then(|v| v.as_str()),
                Some("object"),
                "tool {} must have an object input schema",
                t.name
            );
            assert!(t.description.is_some(), "tool {} needs a description", t.name);
        }
    }
}

//! Native provider adapters ("Capability Plugin / Native Adapter", OAB MCP
//! Adapter ADR §4/§6.1): OAB-side provider integrations that use a provider's
//! REST API when no (usable) hosted MCP server is available.
//!
//! Each adapter is packaged as a **loopback Streamable HTTP MCP server**
//! (`openab gmail-native serve --listen 127.0.0.1:<port>`) — the same
//! transport and trust model as the OAB MCP Facade — rather than a new
//! in-process integration path. The adapter is registered in `mcp.json`
//! like any other `"type": "http"` server, so the runtime's `tool_filter`
//! least-privilege gate, JSON-Schema argument validation, circuit breaker,
//! timeouts, secret redaction, and the facade's discovery/execution surface
//! all apply unchanged — "an implementation path behind the same facade,
//! not a new agent-facing contract".

pub mod gmail;

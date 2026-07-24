//! Native provider adapters ("Capability Plugin / Native Adapter", OAB MCP
//! Adapter ADR §4/§6.1): OAB-side provider integrations that use a provider's
//! REST API when no (usable) hosted MCP server is available.
//!
//! Each adapter is packaged as a **stdio MCP server** (`openab-agent
//! gmail-native serve`) rather than a new in-process integration path. That
//! keeps the ADR's contract intact: the adapter is registered in `mcp.json`
//! like any other server, so the runtime's `tool_filter` least-privilege
//! gate, JSON-Schema argument validation, circuit breaker, timeouts, secret
//! redaction, and the facade's discovery/execution surface all apply
//! unchanged — "an implementation path behind the same facade, not a new
//! agent-facing contract".

pub mod gmail;

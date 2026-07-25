//! Shim: the MCP runtime moved to the shared `openab-mcp` crate so the
//! broker can host the OAB MCP Facade in-process (ADR §6.2). Re-exported
//! here so every existing `crate::mcp::…` path keeps resolving.

pub use openab_mcp::mcp::*;

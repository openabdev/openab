//! Shim: the `auth.json` layer (tenant map, atomic writes, corruption
//! quarantine, MCP credential store, refresh locks) moved to the shared
//! `openab-mcp` crate with the MCP runtime (ADR §6.2). Re-exported here so
//! every existing `crate::auth::…` path keeps resolving.

pub use openab_mcp::auth::*;

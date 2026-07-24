//! OpenAB shared MCP runtime (OAB MCP Adapter ADR, PR #1446).
//!
//! Owns both directions of OpenAB's MCP story:
//!
//! - **Outbound client** (`mcp::runtime` + friends): connections to
//!   configured downstream MCP servers — layered `mcp.json` config, OAuth/
//!   PKCE + `auth.json` credential store, lazy connect, tool cache,
//!   `tool_filter` enforcement, JSON Schema argument validation, timeouts,
//!   circuit breaker, secret redaction.
//! - **Inbound OAB MCP Facade** (`mcp::facade`): the agent-facing MCP server
//!   exposing exactly `search_capabilities` / `execute_capability`, served
//!   over loopback Streamable HTTP by the broker (`[mcp]` in `config.toml`)
//!   or embedded in-process by `openab-agent`.
//!
//! Extracted from `openab-agent` so the broker can host the facade without
//! duplicating the runtime; `openab-agent` re-exports these modules, keeping
//! one implementation (ADR §6.4: one dispatcher, multiple frontends).
//!
//! Module names (`auth`, `llm`, `acp`, `mcp`) intentionally mirror the
//! original `openab-agent` layout so the moved code's `crate::` paths and
//! the agent's `crate::…` re-export shims stay stable.

pub mod acp;
pub mod auth;
pub mod llm;
pub mod mcp;
pub mod native;

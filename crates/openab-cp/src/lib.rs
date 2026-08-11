//! openab-cp — OpenAB Agent Control Plane.
//!
//! Hub-and-spoke registration and routing for direct agent-to-agent
//! delegation, per `docs/adr/agent-control-plane.md`. Runtimes dial out and
//! register over WebSocket; the CP authenticates them against config-bound
//! identities, enforces delegation policy authoritatively, and routes
//! `cp/delegate` / `cp/delegate_result` frames between them.

pub mod config;
pub mod policy;
pub mod proto;
pub mod registry;
pub mod router;
pub mod server;

//! openab-cp — OpenAB Agent Control Plane.
//!
//! Hub-and-spoke registration and routing for direct agent-to-agent
//! delegation, per `docs/adr/agent-control-plane.md`. Runtimes dial out and
//! register over WebSocket; the CP authenticates them against config-bound
//! identities, enforces delegation policy authoritatively, and routes
//! `cp/delegate` / `cp/delegate_result` frames between them.
//!
//! ## Crate layout: `proto` vs the `server` feature
//!
//! [`proto`] is the wire contract and is ALWAYS compiled. Everything else —
//! config, registry, policy, router, observer fan-out, and the WebSocket
//! server — sits behind the default `server` feature, together with the axum
//! and tokio dependencies it needs.
//!
//! That split exists so the OAB runtime (`openab-core`) can depend on this
//! crate with `default-features = false` to speak the protocol without
//! linking an HTTP server it never binds. Anything a runtime client needs
//! belongs in `proto`; anything that only the hub does must stay behind the
//! feature.

#[cfg(feature = "server")]
pub mod config;
#[cfg(feature = "server")]
pub mod events;
#[cfg(feature = "server")]
pub mod policy;
pub mod proto;
#[cfg(feature = "server")]
pub mod registry;
#[cfg(feature = "server")]
pub mod router;
#[cfg(feature = "server")]
pub mod server;

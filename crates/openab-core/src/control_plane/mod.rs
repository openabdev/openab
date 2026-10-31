//! Runtime side of the OpenAB Agent Control Plane (`docs/adr/agent-control-plane.md`).
//!
//! The CP is a hub: runtimes dial *out* to it, register, and hold one
//! WebSocket for the process lifetime. This module is that client — the
//! connection state machine ([`client`]) and the delegation-serving side
//! ([`executor`]) — plus the seam between them and the ACP session pool.
//!
//! Three deliberate boundaries:
//!
//! - **Wire types are not redefined here.** They come from
//!   `openab_cp::proto`, which `openab-core` depends on with
//!   `default-features = false` — the contract without the server.
//! - **Prompt execution is behind a trait** ([`PromptRunner`]), so the client
//!   can be driven end-to-end against a real CP without a real coding agent.
//! - **No cargo feature.** `[control_plane]` in config is the switch; a build
//!   without the section behaves exactly as before.

pub mod client;
pub mod executor;

pub use client::ControlPlaneClient;
pub use executor::{
    delegation_session_key, DelegationExecutor, PromptOutcome, PromptRunner, RouterPromptRunner,
};

use crate::config::CpAgentType;
use openab_cp::proto::AgentType;

/// Widen the runtime's two-variant role into the protocol enum.
///
/// One-way on purpose: `AgentType::Observer` has no runtime counterpart, so
/// there is no `From<AgentType> for CpAgentType` to accidentally admit it.
impl From<CpAgentType> for AgentType {
    fn from(t: CpAgentType) -> Self {
        match t {
            CpAgentType::Primary => AgentType::Primary,
            CpAgentType::Worker => AgentType::Worker,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_roles_map_onto_the_wire_enum() {
        assert_eq!(AgentType::from(CpAgentType::Primary), AgentType::Primary);
        assert_eq!(AgentType::from(CpAgentType::Worker), AgentType::Worker);
    }
}

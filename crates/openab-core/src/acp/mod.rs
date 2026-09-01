#[cfg(feature = "agentcore")]
pub mod agentcore;
pub mod connection;
pub mod pool;
pub mod project;
pub mod protocol;

pub use connection::ContentBlock;
pub use pool::{
    format_native_dispatch_key, is_native_dispatch_key, SessionPool, NATIVE_DISPATCH_KEY_PREFIX,
};
pub use project::ProjectContext;
pub use protocol::{classify_notification, parse_turn_result, AcpEvent, TurnResult};

// Phase 6.4.1F — re-export the READ_ONLY tool-permission gate surface
// so admission + integration tests can drive it through the canonical
// `acp::` namespace without depending on the inner `connection` module.
pub use connection::{
    build_permission_response_with_policy, tool_title_denied_for_read_only, WritePolicyGuard,
    WRITE_POLICY_MODIFY_ALLOWED, WRITE_POLICY_READ_ONLY,
};

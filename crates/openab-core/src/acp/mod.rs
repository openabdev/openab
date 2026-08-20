#[cfg(feature = "agentcore")]
pub mod agentcore;
pub mod connection;
pub mod pool;
pub mod process_tree;
mod windows_env;
pub mod protocol;

pub use connection::ContentBlock;
pub use pool::SessionPool;
pub use protocol::{classify_notification, parse_turn_result, AcpEvent, TurnResult};

pub use windows_env::WINDOWS_RUNTIME_ENV_KEYS;

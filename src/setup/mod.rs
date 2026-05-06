//! OpenAB interactive setup wizard.
//!
//! Modules:
//! - `validate` — input validation (bot token, channel ID, agent command)
//! - `config`   — TOML config generation and serialization
//! - `wizard`   — interactive TUI, Discord API client, and wizard entry point

mod config;
mod validate;
mod wizard;

pub use wizard::run_setup;

//! shepherd — agent orchestration engine.
//!
//! The library half of the `shep` binary. Every write to the store goes through
//! [`engine`]; the CLI and the supervisor are the same code over the same SQLite
//! file, so consistency comes from sharing these functions rather than from a
//! transport (PLAN §7.4).

pub mod cmd;
pub mod db;
pub mod engine;
pub mod error;
pub mod logging;
pub mod paths;
pub mod supervisor;

pub use error::{Error, Result};

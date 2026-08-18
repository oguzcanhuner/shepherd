//! shepherd — agent orchestration engine.
//!
//! The library half of the `shep` binary. Every write to the store goes through
//! [`engine`]; the CLI and the supervisor are the same code over the same SQLite
//! file, so consistency comes from sharing these functions rather than from a
//! transport.

pub mod cmd;
pub mod config;
pub mod db;
pub mod engine;
pub mod error;
pub mod git;
pub mod logging;
pub mod outcome;
pub mod paths;
pub mod supervisor;

pub use error::{Error, Result};
pub use outcome::Outcome;

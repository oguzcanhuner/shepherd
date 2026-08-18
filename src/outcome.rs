//! The outcome vocabulary shared by the step contract (PLAN §7.1) and by config
//! (`on_exhausted`). Four words, fixed: adding a fifth would change what every
//! step script is allowed to say.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Step succeeded. Advance.
    Pass,
    /// The step's verdict is negative. Take the pipeline's `on_fail`.
    Reject,
    /// A promise, not an answer: resolve later per the pipeline's `await`.
    Started,
    /// Something broke. Park the task.
    Error,
}

impl Outcome {
    pub const ALL: [Outcome; 4] = [
        Outcome::Pass,
        Outcome::Reject,
        Outcome::Started,
        Outcome::Error,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Pass => "pass",
            Outcome::Reject => "reject",
            Outcome::Started => "started",
            Outcome::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Result<Outcome> {
        Outcome::ALL
            .into_iter()
            .find(|o| o.as_str() == s)
            .ok_or_else(|| Error::other(format!("unknown outcome {s:?}")))
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

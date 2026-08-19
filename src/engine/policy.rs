//! Reading a task's policy. Await values are config's business, but the engine
//! needs to name them.

use crate::config::Policy;
use crate::db::task::Task;
use crate::{Error, Result};
use std::path::Path;

/// The policy governing a task, loaded from the repo it belongs to.
///
/// Loaded per task rather than once, because config is per repo root
/// and two tasks in flight may be governed by different files.
pub fn policy_for(task: &Task) -> Result<Policy> {
    Policy::load(Path::new(&task.repo))
        .map_err(|e| Error::other(format!("task {} cannot run: {e}", task.id)))
}

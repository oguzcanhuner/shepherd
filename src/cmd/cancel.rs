use crate::db;
use crate::engine::{self, TransitionOutcome};
use anyhow::{Result, bail};
use std::path::Path;

/// `shep cancel <task>` — stop a task for good.
///
/// Tearing down the pane, worktree and workspace that a cancelled task owns lands
/// with integrate and teardown in M8; until then this stops the state machine and
/// leaves anything Herdr-side alone.
pub fn run(db_path: &Path, task_id: &str, reason: Option<String>) -> Result<()> {
    let mut conn = db::open(db_path)?;
    match engine::cancel(&mut conn, task_id, reason)? {
        TransitionOutcome::Applied(_) => {
            println!("{task_id} cancelled");
            Ok(())
        }
        TransitionOutcome::Bailed(reason) => bail!("{task_id}: {reason}"),
    }
}

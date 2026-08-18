use crate::db;
use crate::engine::{self, TransitionOutcome};
use anyhow::{Result, bail};
use std::path::Path;

/// `shep retry <task>` — re-queue a parked task. A stuck task sits there until
/// this is run; the failure is inert, which is why there is no reconciliation
/// loop.
pub fn run(db_path: &Path, task_id: &str) -> Result<()> {
    let mut conn = db::open(db_path)?;
    match engine::retry(&mut conn, task_id)? {
        TransitionOutcome::Applied(applied) => {
            let position = match (&applied.task.pipeline, &applied.task.step) {
                (Some(p), Some(s)) => format!(" at {p}/{s}"),
                _ => String::new(),
            };
            println!("{task_id} queued again{position}");
            Ok(())
        }
        TransitionOutcome::Bailed(reason) => bail!("{task_id}: {reason}"),
    }
}

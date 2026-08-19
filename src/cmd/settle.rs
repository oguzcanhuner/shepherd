use crate::engine::{self, TransitionOutcome};
use crate::{config::Policy, db, db::task};
use anyhow::{Result, bail};
use std::path::Path;

/// `shep run <pipeline>` — send a task back through a pipeline by hand.
pub fn run_pipeline(db_path: &Path, pipeline: &str, task: Option<String>) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let task_id = super::task_id(&conn, task)?;
    let row = task::require(&conn, &task_id)?;
    let policy = Policy::load(Path::new(&row.repo))?;

    match engine::run_pipeline(&mut conn, &policy, &task_id, pipeline)? {
        TransitionOutcome::Applied(applied) => {
            println!(
                "{task_id} is queued at {pipeline}/{}",
                applied.task.step.as_deref().unwrap_or("?")
            );
            Ok(())
        }
        TransitionOutcome::Bailed(reason) => bail!("{task_id}: {reason}"),
    }
}

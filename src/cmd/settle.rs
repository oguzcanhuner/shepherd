use crate::db::check::Conclusion;
use crate::engine::{self, TransitionOutcome};
use crate::{config::Policy, db, db::task};
use anyhow::{Result, bail};
use std::io::Read;
use std::path::Path;

/// `shep approve` / `shep reject` — the only things that resolve a handoff.
pub fn run(
    db_path: &Path,
    conclusion: Conclusion,
    task: Option<String>,
    author: Option<String>,
    note: Option<String>,
) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let task_id = super::task_id(&conn, task)?;
    let row = task::require(&conn, &task_id)?;
    let policy = Policy::load(Path::new(&row.repo))?;

    let author = author.unwrap_or_else(whoami);
    let note = note.or(read_stdin()?);
    let (check, moved) =
        engine::settle_by_human(&mut conn, &policy, &task_id, conclusion, &author, note)?;

    match moved {
        TransitionOutcome::Applied(applied) => {
            let now = &applied.task;
            let where_now = match (&now.pipeline, &now.step) {
                (Some(p), Some(s)) => format!("{p}/{s}"),
                _ => now.status.to_string(),
            };
            println!(
                "{task_id} {} as {} ({where_now})",
                conclusion.as_str(),
                check.id
            );
            Ok(())
        }
        // The check is written either way: it was true when you said it.
        TransitionOutcome::Bailed(reason) => bail!("{task_id} recorded {}: {reason}", check.id),
    }
}

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

fn whoami() -> String {
    std::env::var("USER")
        .ok()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "you".to_string())
}

/// A longer note, when there is one. Only from a pipe: an approval typed at a
/// prompt must not sit there waiting for EOF.
fn read_stdin() -> Result<Option<String>> {
    if unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
        return Ok(None);
    }
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    let body = buf.trim();
    Ok((!body.is_empty()).then(|| body.to_string()))
}

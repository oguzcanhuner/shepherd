use crate::db::{self, task};
use anyhow::Result;
use std::path::Path;

/// `shep context` — my brief.
///
/// One of exactly three things an agent in a pane knows about (PLAN §7.5), and a
/// pure read against a database file: there is nothing here for an agent to
/// connect to and nothing to be down.
pub fn run(db_path: &Path, task: Option<String>, json: bool) -> Result<()> {
    let conn = db::open_existing(db_path)?;
    let task_id = super::task_id(&conn, task)?;
    let task = task::require(&conn, &task_id)?;
    let pane = db::pane::for_task(&conn, &task.id)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": task.id,
                "brief": task.brief,
                "type": task.kind,
                "pipeline": task.pipeline,
                "step": task.step,
                "round": task.round,
                "status": task.status.as_str(),
                "repo": task.repo,
                "worktree": task.worktree,
                "branch": task.branch,
                "base": task.base,
                "pane": pane,
            }))?
        );
        return Ok(());
    }

    // Facts first and short, then the brief verbatim and last: a long brief must
    // not push what a step needs to know off the top of the pane.
    println!(
        "task {} ({}), at {}/{} round {}",
        task.id,
        task.kind,
        task.pipeline.as_deref().unwrap_or("-"),
        task.step.as_deref().unwrap_or("-"),
        task.round
    );
    match (&task.branch, &task.base) {
        (Some(branch), Some(base)) => println!("branch {branch} off {base}"),
        (Some(branch), None) => println!("branch {branch}"),
        _ => {}
    }
    println!(
        "working in {}",
        task.worktree.as_deref().unwrap_or(&task.repo)
    );
    println!("\nbrief\n\n{}", task.brief);
    Ok(())
}

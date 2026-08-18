use crate::db;
use crate::engine::{self, Binding, TransitionOutcome};
use anyhow::{Result, bail};
use std::path::Path;

/// `shep bind-pane wA:p2` — say where a task's work is happening.
///
/// Run by a step script once it has a pane, before it starts an agent in it. The
/// binding is what makes a Herdr event attributable to a task, so it has to exist
/// before the agent does — otherwise the first status change is about a pane
/// nobody claims.
#[allow(clippy::too_many_arguments)]
pub fn run(
    db_path: &Path,
    pane: &str,
    task: Option<String>,
    workspace: Option<String>,
    worktree: Option<String>,
    branch: Option<String>,
    base: Option<String>,
) -> Result<()> {
    let mut conn = db::open(db_path)?;
    let task_id = super::task_id(&conn, task)?;
    let binding = Binding {
        pane: pane.to_string(),
        workspace,
        worktree,
        branch,
        base,
    };
    match engine::bind_pane(&mut conn, &task_id, &binding)? {
        TransitionOutcome::Applied(_) => {
            println!("{pane} is {task_id}");
            Ok(())
        }
        TransitionOutcome::Bailed(reason) => bail!("{task_id}: {reason}"),
    }
}

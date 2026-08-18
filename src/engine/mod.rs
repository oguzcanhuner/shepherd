//! The state machine. Every write to a task goes through here, because the CLI
//! and the supervisor are both writers and consistency comes from shared code
//! rather than from a transport (PLAN §7.4).

mod transition;

pub use transition::{Applied, Decision, Outcome, transition};

use crate::Result;
use crate::db::{self, event, task};
use rusqlite::Connection;

/// Create a task: one `BEGIN IMMEDIATE` transaction that allocates the id,
/// inserts the row and writes `task.created`. State and event commit together,
/// always (PLAN §6).
pub fn create_task(conn: &mut Connection, new: task::NewTask) -> Result<task::Task> {
    let tx = db::write_tx(conn)?;
    let now = db::now();
    let task = task::Task {
        id: task::next_id(&tx)?,
        brief: new.brief,
        kind: new.kind,
        pipeline: None,
        step: None,
        round: 0,
        status: task::Status::Queued,
        human_owned: false,
        repo: new.repo,
        worktree: None,
        branch: None,
        base: None,
        workspace_id: None,
        created: now,
        updated: now,
    };
    task::insert(&tx, &task)?;
    event::append(
        &tx,
        &event::NewEvent::for_task(event::names::TASK_CREATED, &task.id).payload(
            serde_json::json!({
                "type": task.kind,
                "repo": task.repo,
                "brief": task.brief,
            }),
        ),
    )?;
    tx.commit()?;
    tracing::info!(task = %task.id, kind = %task.kind, "task created");
    Ok(task)
}

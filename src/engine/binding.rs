//! Binding a Herdr pane to a task.

use super::transition::{Decision, Outcome as TransitionOutcome, transition_with};
use crate::db::event::{NewEvent, names};
use crate::db::pane;
use crate::db::task::TaskPatch;
use crate::Result;
use rusqlite::Connection;

/// Where a task's work is happening. Everything but the pane is optional,
/// because absent means "leave what is there": re-binding a pane for a second
/// round must not erase the worktree the first round created.
#[derive(Debug, Clone)]
pub struct Binding {
    pub pane: String,
    pub workspace: Option<String>,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub base: Option<String>,
}

impl Binding {
    pub fn to(pane: impl Into<String>) -> Binding {
        Binding {
            pane: pane.into(),
            workspace: None,
            worktree: None,
            branch: None,
            base: None,
        }
    }
}

/// `shep bind-pane` — bind a pane to a task, with the worktree it works in.
///
/// One transaction for the `pane_task` row, the task's placement and the event.
/// A task that thinks it has a worktree but has no pane bound is a state nothing
/// recovers from, and the pane binding is what makes a Herdr event attributable
/// at all, so the two must not be separable.
pub fn bind_pane(
    conn: &mut Connection,
    task_id: &str,
    binding: &Binding,
) -> Result<TransitionOutcome> {
    transition_with(conn, task_id, |tx, task| {
        if task.status.is_terminal() {
            return Ok(Decision::bail(format!(
                "task is {} — nothing should be starting work for it",
                task.status
            )));
        }
        pane::bind(tx, &binding.pane, task_id)?;

        let mut patch = TaskPatch::new();
        if binding.workspace.is_some() {
            patch = patch.workspace_id(binding.workspace.clone());
        }
        if binding.worktree.is_some() {
            patch = patch.worktree(binding.worktree.clone());
        }
        if binding.branch.is_some() {
            patch = patch.branch(binding.branch.clone());
        }
        if binding.base.is_some() {
            patch = patch.base(binding.base.clone());
        }

        Ok(Decision::apply(patch).with_event(
            NewEvent::for_task(names::TASK_PANE_BOUND, task_id).payload(serde_json::json!({
                "pane": binding.pane,
                "workspace": binding.workspace,
                "worktree": binding.worktree,
                "branch": binding.branch,
                "base": binding.base,
            })),
        ))
    })
}

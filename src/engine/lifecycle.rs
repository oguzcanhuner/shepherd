//! A task's lifecycle from the outside: create, park, retry, re-run, cancel.
//!
//! These are the verbs the CLI exposes. Each one is a single transition (or a
//! single transaction), so a command either happened entirely or not at all.

use super::flow::park;
use super::transition::{Decision, Outcome as TransitionOutcome, transition};
use crate::config::Policy;
use crate::db::event::{NewEvent, names};
use crate::db::task::{Status, Task, TaskPatch};
use crate::db::{self, event, task};
use crate::{Error, Result};
use rusqlite::Connection;

/// Create a task: one `BEGIN IMMEDIATE` transaction that allocates the id,
/// inserts the row and writes `task.created`. State and event commit together,
/// always.
pub fn create_task(conn: &mut Connection, new: task::NewTask) -> Result<Task> {
    let tx = db::write_tx(conn)?;
    let now = db::now();
    let task = Task {
        id: task::next_id(&tx)?,
        brief: new.brief,
        kind: new.kind,
        pipeline: None,
        step: None,
        round: 0,
        status: Status::Queued,
        human_owned: false,
        plan: new.plan,
        await_deadline: None,
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
        &NewEvent::for_task(names::TASK_CREATED, &task.id).payload(serde_json::json!({
            "type": task.kind,
            "repo": task.repo,
            "brief": task.brief,
        })),
    )?;
    tx.commit()?;
    tracing::info!(task = %task.id, kind = %task.kind, "task created");
    Ok(task)
}

/// Park a task from outside a step: a policy that will not load, or anything else
/// the engine cannot decide its way past.
pub fn park_task(conn: &mut Connection, task_id: &str, reason: &str) -> Result<TransitionOutcome> {
    transition(conn, task_id, |task| {
        if task.status == Status::Parked || task.status.is_terminal() {
            return Ok(Decision::bail(format!("already {}", task.status)));
        }
        Ok(park(reason))
    })
}

/// Re-queue a parked task so the supervisor picks it up again, retrying the step
/// it stopped on.
pub fn retry(conn: &mut Connection, task_id: &str) -> Result<TransitionOutcome> {
    transition(conn, task_id, |task| {
        if task.status != Status::Parked {
            return Ok(Decision::bail(format!(
                "only a parked task can be retried, and this one is {}",
                task.status
            )));
        }
        Ok(
            Decision::apply(TaskPatch::new().status(Status::Queued)).with_event(
                NewEvent::new(names::TASK_RESUMED).payload(serde_json::json!({
                    "pipeline": task.pipeline,
                    "step": task.step,
                    "round": task.round,
                })),
            ),
        )
    })
}

/// `shep run <pipeline>` — put a task at the top of a pipeline, out of band.
///
/// For the handoff you are in the middle of: read the diff, decide the review
/// should run again, and send it back yourself. Where it goes afterwards is
/// ordinary — the pipeline passes and the type carries on from there, which for
/// `review` means straight back to the handoff you were standing in.
pub fn run_pipeline(
    conn: &mut Connection,
    policy: &Policy,
    task_id: &str,
    pipeline: &str,
) -> Result<TransitionOutcome> {
    let _ = task::require(conn, task_id)?;
    // Any defined pipeline can be applied by hand — it need not be in the type
    // that seeded the task, because "what's next" now lives on the row, not in
    // the type. The applied pipeline becomes the task's plan; when it finishes,
    // the task comes to rest.
    let first = policy
        .pipeline(pipeline)?
        .steps
        .first()
        .ok_or_else(|| Error::other(format!("pipeline {pipeline:?} has no steps")))?
        .name()
        .to_string();

    transition(conn, task_id, |task| {
        if task.status.is_terminal() {
            return Ok(Decision::bail(format!(
                "already {} — create a new task instead",
                task.status
            )));
        }
        // Move the position to this pipeline. If it is already in the plan (a
        // re-run), leave the plan be so the task continues its sequence
        // afterwards; if it is new (applied to a resting task, or one the type
        // never listed), append it so it has somewhere to return to — then rest.
        let mut plan = task.plan.clone();
        if !plan.iter().any(|p| p == pipeline) {
            plan.push(pipeline.to_string());
        }
        Ok(Decision::apply(
            TaskPatch::new()
                .status(Status::Queued)
                .pipeline(Some(pipeline))
                .step(Some(first.clone()))
                .round(0)
                .plan(plan)
                // Asking for a pipeline by hand is handing the task back.
                .human_owned(false),
        )
        .with_event(
            NewEvent::new(names::TASK_RESUMED).payload(serde_json::json!({
                "reason": "run by hand",
                "pipeline": pipeline,
                "step": first,
                "was": {"pipeline": task.pipeline, "step": task.step, "round": task.round},
            })),
        ))
    })
}

/// Stop a task for good.
pub fn cancel(
    conn: &mut Connection,
    task_id: &str,
    reason: Option<String>,
) -> Result<TransitionOutcome> {
    transition(conn, task_id, |task| {
        if task.status.is_terminal() {
            return Ok(Decision::bail(format!("already {}", task.status)));
        }
        Ok(
            Decision::apply(TaskPatch::new().status(Status::Cancelled)).with_event(
                NewEvent::new(names::TASK_CANCELLED)
                    .payload(serde_json::json!({"reason": reason, "was": task.status.as_str()})),
            ),
        )
    })
}

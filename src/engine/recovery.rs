//! Recovery after a supervisor died mid-step.

use super::policy::policy_for;
use super::step::StepAt;
use super::transition::{Decision, transition};
use crate::db::event::{NewEvent, names};
use crate::db::task::{Status, Task, TaskPatch};
use crate::db::{pane, task};
use crate::Result;
use rusqlite::Connection;

/// Requeue steps that were in flight when the supervisor died.
///
/// A synchronous step died with the supervisor and has to be run again; a
/// deferred one is still out there and must be left alone. The original design read
/// that difference off the pane binding, but a task keeps its agent pane after
/// `implement` resolves — `shep context` has to keep working in it, and `handoff`
/// wants to talk to the same agent — so the config is the better witness: what a
/// step is waiting for is exactly what its pipeline's `await` says.
///
/// The pane binding is still the fallback for a task whose policy will not load,
/// since then nothing else can say.
pub fn recover_orphans(conn: &mut Connection) -> Result<Vec<String>> {
    let candidates = task::list_by_status(conn, Status::Running)?;
    let mut recovered = Vec::new();
    for candidate in candidates {
        if is_deferred(conn, &candidate)? {
            continue;
        }
        let outcome = transition(conn, &candidate.id, |task| {
            if task.status != Status::Running {
                return Ok(Decision::bail("no longer running"));
            }
            Ok(
                Decision::apply(TaskPatch::new().status(Status::Queued)).with_event(
                    NewEvent::new(names::TASK_RESUMED).payload(serde_json::json!({
                        "reason": "orphaned by a supervisor that stopped mid-step",
                        "pipeline": task.pipeline,
                        "step": task.step,
                        "round": task.round,
                    })),
                ),
            )
        })?;
        if outcome.is_applied() {
            tracing::warn!(task = %candidate.id, step = ?candidate.step, "re-queued an orphaned step");
            recovered.push(candidate.id);
        }
    }
    Ok(recovered)
}

/// Is this running task waiting on something outside the supervisor — an agent,
/// or a human — rather than on a step script that died with it?
fn is_deferred(conn: &Connection, task: &Task) -> Result<bool> {
    let Some(at) = StepAt::of(task) else {
        return Ok(false);
    };
    match policy_for(task) {
        Ok(policy) => Ok(policy
            .config
            .pipeline
            .get(&at.pipeline)
            .and_then(|p| p.await_on)
            .is_some()),
        Err(e) => {
            tracing::warn!(task = %task.id, "cannot tell what {at} is waiting for: {e}");
            Ok(pane::for_task(conn, &task.id)?.is_some())
        }
    }
}

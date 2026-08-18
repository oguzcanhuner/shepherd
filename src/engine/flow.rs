//! The step flow: starting a task's next step and recording what it said.
//!
//! This is the core of the state machine. Everything else in the engine —
//! lifecycle commands, human settlement, recovery — funnels into
//! [`begin_step`] and [`finish_step`].

use super::plan::{self, Plan};
use super::step::{StepAt, StepReport, StepSpec};
use super::transition::{Decision, Outcome as TransitionOutcome, transition};
use crate::Outcome;
use crate::config::{Await, Policy};
use crate::db::event::{NewEvent, names};
use crate::db::task::{Status, TaskPatch};
use crate::db::{self};
use crate::Result;
use rusqlite::Connection;
use std::path::Path;

/// What starting a task's next step turned out to mean.
#[derive(Debug, Clone)]
pub enum Started {
    /// A step is now in flight; run this and report back with [`finish_step`].
    Running(Box<StepSpec>),
    /// The type's pipelines are all done.
    Finished,
    /// The task is parked and will not move until `shep retry`.
    Parked { reason: String },
    /// The row moved before the lock was taken; someone else got there first.
    Bailed { reason: String },
}

/// Start the next step of a queued task.
///
/// `queued` means "ready to run its next step" and `running` means "a step is in
/// flight right now". Keeping those apart is what makes recovery decidable: a
/// task left `running` with no pane was synchronous and got orphaned.
pub fn begin_step(
    conn: &mut Connection,
    policy: &Policy,
    task_id: &str,
    db_path: &Path,
) -> Result<Started> {
    let mut chosen: Option<Plan> = None;
    let outcome = transition(conn, task_id, |task| {
        if task.status != Status::Queued {
            return Ok(Decision::bail(format!(
                "not queued any more (it is {})",
                task.status
            )));
        }

        // A queued task with a position already recorded is one whose previous
        // step passed: the position *is* the next step. Planning only happens
        // when there is nowhere yet.
        let plan = match (task.pipeline.clone(), task.step.clone()) {
            (Some(pipeline), Some(step)) => Plan::Run {
                pipeline,
                step,
                round: task.round,
            },
            _ => plan::start(policy, task),
        };
        chosen = Some(plan.clone());
        Ok(start_or_settle(&plan))
    })?;

    let applied = match outcome {
        TransitionOutcome::Bailed(reason) => return Ok(Started::Bailed { reason }),
        TransitionOutcome::Applied(applied) => applied,
    };

    match chosen.expect("a decision was applied, so one was made") {
        Plan::Run {
            pipeline,
            step,
            round,
        } => {
            let pane = db::pane::for_task(conn, task_id)?;
            let spec = StepSpec::resolve(
                policy,
                &applied.task,
                &pipeline,
                &step,
                round,
                db_path,
                pane,
            )?;
            Ok(Started::Running(Box::new(spec)))
        }
        Plan::Finish => Ok(Started::Finished),
        Plan::Park { reason } => Ok(Started::Parked { reason }),
    }
}

/// Record what a step said, and move the task accordingly.
///
/// One transaction: the step finishing and whatever it leads to commit together,
/// so there is never an event for a change that didn't persist.
pub fn finish_step(
    conn: &mut Connection,
    policy: &Policy,
    task_id: &str,
    at: &StepAt,
    report: &StepReport,
) -> Result<TransitionOutcome> {
    transition(conn, task_id, |task| {
        // The guard: this thread is reporting on a specific step of a specific
        // round. If any of that has moved, the report is stale.
        if task.status != Status::Running || StepAt::of(task).as_ref() != Some(at) {
            return Ok(Decision::bail(format!(
                "task moved on: it is {} at {:?}/{:?} round {}, not {at}",
                task.status, task.pipeline, task.step, task.round,
            )));
        }

        let finished =
            NewEvent::for_task(names::TASK_STEP_FINISHED, task_id).payload(serde_json::json!({
                "pipeline": at.pipeline,
                "step": at.step,
                "round": at.round,
                "outcome": report.outcome.as_str(),
                "note": report.note,
            }));

        let decision = match report.outcome {
            Outcome::Pass => advance_to(&plan::after_pass(policy, task)),

            Outcome::Error => park(format!(
                "step {} errored: {}",
                at.step,
                report.note.as_deref().unwrap_or("no reason given")
            )),

            // A rejection is a verdict, not a failure: where it goes is the
            // pipeline's `on_fail`, bounded by its `max_rounds`.
            Outcome::Reject => advance_to(&plan::after_fail(policy, task)),

            // A promise, not an answer. What resolves it is the pipeline's await.
            Outcome::Started => {
                let awaits = policy
                    .config
                    .pipeline
                    .get(&at.pipeline)
                    .and_then(|p| p.await_on);
                match awaits {
                    // `human_owned` is the muting: status events for its pane are
                    // still written to `raw_event`, but advance nothing, so you can
                    // talk to the agent without the state machine moving under you.
                    Some(await_on) => {
                        Decision::apply(TaskPatch::new().human_owned(await_on == Await::Human))
                            .with_event(
                                NewEvent::for_task(names::TASK_STEP_AWAITING, task_id).payload(
                                    serde_json::json!({
                                        "pipeline": at.pipeline,
                                        "step": at.step,
                                        "round": at.round,
                                        "await": await_on.as_str(),
                                        "pane": report.pane,
                                    }),
                                ),
                            )
                    }
                    None => park(format!(
                        "step {} returned \"started\", but pipeline {} has no await, so nothing \
                         would ever resolve it",
                        at.step, at.pipeline
                    )),
                }
            }
        };

        // Any answer to a step ends the muting, whatever the answer was: the task
        // is the machine's again the moment it is not waiting for you.
        let decision = match report.outcome {
            Outcome::Started => decision,
            _ => decision.map_patch(|patch| patch.human_owned(false)),
        };

        // The step_finished event comes first: it is the record of what happened,
        // and what follows is the consequence.
        Ok(decision.preceded_by(finished))
    })
}

/// Move a task to where it goes next without starting it.
///
/// The next step is left `queued`, and the next tick starts it. That keeps the
/// two statuses meaning one thing each — `queued` is "ready to run its next
/// step", `running` is "a step is in flight" — which is what makes orphan
/// recovery decidable. The cost is one poll interval between steps.
fn advance_to(plan: &Plan) -> Decision {
    match plan {
        Plan::Run {
            pipeline,
            step,
            round,
        } => Decision::apply(
            TaskPatch::new()
                .status(Status::Queued)
                .pipeline(Some(pipeline.clone()))
                .step(Some(step.clone()))
                .round(*round),
        ),
        // These end the task, so they are the same either way.
        Plan::Finish | Plan::Park { .. } => start_or_settle(plan),
    }
}

/// Turn a plan into the state change that starts it.
fn start_or_settle(plan: &Plan) -> Decision {
    match plan {
        Plan::Run {
            pipeline,
            step,
            round,
        } => Decision::apply(
            TaskPatch::new()
                .status(Status::Running)
                .pipeline(Some(pipeline.clone()))
                .step(Some(step.clone()))
                .round(*round),
        )
        .with_event(
            NewEvent::new(names::TASK_STEP_STARTED).payload(serde_json::json!({
                "pipeline": pipeline,
                "step": step,
                "round": round,
            })),
        ),
        Plan::Finish => Decision::apply(
            TaskPatch::new()
                .status(Status::Finished)
                .step(None::<String>)
                .pipeline(None::<String>),
        )
        .with_event(NewEvent::new(names::TASK_FINISHED)),
        Plan::Park { reason } => park(reason.clone()),
    }
}

/// Parking is the answer to everything the engine cannot decide. It is inert: the
/// task sits there until `shep retry`.
pub(super) fn park(reason: impl Into<String>) -> Decision {
    let reason = reason.into();
    Decision::apply(TaskPatch::new().status(Status::Parked)).with_event(
        NewEvent::new(names::TASK_PARKED).payload(serde_json::json!({"reason": reason})),
    )
}

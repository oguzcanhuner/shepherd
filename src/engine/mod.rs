//! The state machine. Every write to a task goes through here, because the CLI
//! and the supervisor are both writers and consistency comes from shared code
//! rather than from a transport (PLAN §7.4).

mod plan;
pub mod resolve;
mod step;
mod transition;

pub use plan::Plan;
pub use resolve::{AgentStatus, Drained, drain};
pub use step::{StepAt, StepReport, StepSpec, environment, run as run_step};
pub use transition::{
    Applied, Decision, Outcome as TransitionOutcome, transition, transition_with,
};

use crate::Outcome;
use crate::config::{Await, Policy};
use crate::db::check::{Check, Conclusion, NewCheck};
use crate::db::event::{NewEvent, names};
use crate::db::task::{Status, Task, TaskPatch};
use crate::db::{self, check, event, pane, task};
use crate::{Error, Result};
use rusqlite::Connection;
use std::path::Path;

/// Create a task: one `BEGIN IMMEDIATE` transaction that allocates the id,
/// inserts the row and writes `task.created`. State and event commit together,
/// always (PLAN §6).
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
/// task left `running` with no pane was synchronous and got orphaned (PLAN §11).
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
/// so there is never an event for a change that didn't persist (PLAN §6).
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

            // M6 wires on_fail, max_rounds and on_exhausted. Until then a
            // rejection has nowhere to go.
            Outcome::Reject => {
                let on_fail = policy
                    .config
                    .pipeline
                    .get(&at.pipeline)
                    .and_then(|p| p.on_fail.clone());
                park(match on_fail {
                    Some(target) => format!(
                        "step {} rejected; the on_fail loop to {target:?} lands in M6",
                        at.step
                    ),
                    None => format!(
                        "step {} rejected and pipeline {} has no on_fail",
                        at.step, at.pipeline
                    ),
                })
            }

            // A promise, not an answer. What resolves it is the pipeline's await.
            Outcome::Started => {
                let awaits = policy
                    .config
                    .pipeline
                    .get(&at.pipeline)
                    .and_then(|p| p.await_on);
                match awaits {
                    Some(await_on) => Decision::apply(TaskPatch::new()).with_event(
                        NewEvent::for_task(names::TASK_STEP_AWAITING, task_id).payload(
                            serde_json::json!({
                                "pipeline": at.pipeline,
                                "step": at.step,
                                "round": at.round,
                                "await": await_on.as_str(),
                                "pane": report.pane,
                            }),
                        ),
                    ),
                    None => park(format!(
                        "step {} returned \"started\", but pipeline {} has no await, so nothing \
                         would ever resolve it",
                        at.step, at.pipeline
                    )),
                }
            }
        };

        // The step_finished event comes first: it is the record of what happened,
        // and what follows is the consequence.
        Ok(prepend_event(decision, finished))
    })
}

/// Move a task to where it goes next without starting it.
///
/// The next step is left `queued`, and the next tick starts it. That keeps the
/// two statuses meaning one thing each — `queued` is "ready to run its next
/// step", `running` is "a step is in flight" — which is what makes orphan
/// recovery decidable (PLAN §11). The cost is one poll interval between steps.
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
/// task sits there until `shep retry` (PLAN §1).
fn park(reason: impl Into<String>) -> Decision {
    let reason = reason.into();
    Decision::apply(TaskPatch::new().status(Status::Parked)).with_event(
        NewEvent::new(names::TASK_PARKED).payload(serde_json::json!({"reason": reason})),
    )
}

fn prepend_event(decision: Decision, first: NewEvent) -> Decision {
    match decision {
        Decision::Apply { patch, events } => {
            let mut all = vec![first];
            all.extend(events);
            Decision::Apply { patch, events: all }
        }
        bail => bail,
    }
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
/// at all (PLAN §6), so the two must not be separable.
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

/// What a check submitter supplies (PLAN §7.3). Notably not the sha.
#[derive(Debug, Clone)]
pub struct Submission {
    pub conclusion: Conclusion,
    /// Who is judging: a step name, a tool, or a person.
    pub author: Option<String>,
    pub body: Option<String>,
    /// The position being judged, when the caller's environment says. Absent
    /// means "wherever the task is now", which is what an agent in a pane gets:
    /// its shell's environment was fixed when the pane was split and would go
    /// stale the moment the round changed.
    pub at: Option<StepAt>,
}

/// `shep check submit` — a verdict plus evidence about a specific commit.
///
/// `shep` stamps the sha itself, from `git rev-parse HEAD` in the worktree. The
/// submitter never supplies it, or a stale check becomes an agent-behaviour bug
/// instead of an impossible state (PLAN §7.3).
pub fn submit_check(conn: &mut Connection, task_id: &str, sub: &Submission) -> Result<Check> {
    let task = task::require(conn, task_id)?;
    let at = sub.at.clone().or_else(|| StepAt::of(&task));
    let author = sub
        .author
        .clone()
        .or_else(|| at.as_ref().map(|a| a.step.clone()))
        .unwrap_or_else(|| "anonymous".to_string());

    // The worktree if there is one, else the repo: a check about a task that
    // never got a worktree of its own is still a check about a commit.
    let dir = task.worktree.clone().unwrap_or_else(|| task.repo.clone());
    // Outside the transaction, on purpose: never hold one open across a
    // subprocess (PLAN §7.4).
    let sha = crate::git::head_sha(Path::new(&dir))?;

    let new = NewCheck {
        task_id: task_id.to_string(),
        pipeline: at.as_ref().map(|a| a.pipeline.clone()),
        step: at.as_ref().map(|a| a.step.clone()),
        round: at.as_ref().map(|a| a.round),
        author,
        sha,
        conclusion: sub.conclusion,
        body: sub.body.clone(),
    };

    let tx = db::write_tx(conn)?;
    let written = check::insert(&tx, &new)?;
    event::append(
        &tx,
        &NewEvent::for_task(names::TASK_CHECK_SUBMITTED, task_id).payload(serde_json::json!({
            "check": written.id,
            "author": written.author,
            "conclusion": written.conclusion.as_str(),
            "sha": written.sha,
            "pipeline": written.pipeline,
            "step": written.step,
            "round": written.round,
        })),
    )?;
    tx.commit()?;
    tracing::info!(
        task = %task_id, check = %written.id, author = %written.author,
        conclusion = written.conclusion.as_str(), "check submitted"
    );
    Ok(written)
}

/// Requeue steps that were in flight when the supervisor died.
///
/// A synchronous step died with the supervisor and has to be run again; a
/// deferred one is still out there and must be left alone (PLAN §11). PLAN reads
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

/// Await values are config's business, but the engine needs to name them.
pub fn awaits_human(policy: &Policy, pipeline: &str) -> bool {
    policy
        .config
        .pipeline
        .get(pipeline)
        .and_then(|p| p.await_on)
        == Some(Await::Human)
}

/// The policy governing a task, loaded from the repo it belongs to.
///
/// Loaded per task rather than once, because config is per repo root (PLAN §4)
/// and two tasks in flight may be governed by different files.
pub fn policy_for(task: &Task) -> Result<Policy> {
    Policy::load(Path::new(&task.repo))
        .map_err(|e| Error::other(format!("task {} cannot run: {e}", task.id)))
}

//! Checks: verdicts about specific commits.
//!
//! A check is how anything — a linter, a reviewing agent, a person — says
//! pass or fail about the exact commit it looked at. `shep` stamps the sha
//! itself, so a submitter can never pin a verdict to code it did not judge.

use super::flow::finish_step;
use super::step::{StepAt, StepReport};
use super::transition::Outcome as TransitionOutcome;
use crate::Outcome;
use crate::config::Policy;
use crate::db::check::{Check, Conclusion, NewCheck};
use crate::db::event::{NewEvent, names};
use crate::db::task::Status;
use crate::db::{self, check, event, task};
use crate::{Error, Result};
use rusqlite::Connection;
use std::path::Path;

/// What a check submitter supplies. Notably not the sha.
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
/// instead of an impossible state.
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
    // subprocess.
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

/// `shep signal <task> --name <sig>` — resolve a step that is awaiting a named
/// signal, built-in or declared. The verdict rides on the signal (an external
/// system is the authority on its own result), recorded as a check for
/// provenance, then the step is finished with it — the same path a human verdict
/// takes.
pub fn settle_by_signal(
    conn: &mut Connection,
    policy: &Policy,
    task_id: &str,
    signal: &str,
    conclusion: Conclusion,
    author: &str,
    note: Option<String>,
) -> Result<(Check, TransitionOutcome)> {
    let task = task::require(conn, task_id)?;
    let at = StepAt::of(&task).filter(|at| {
        task.status == Status::Running && policy.step_await(&at.pipeline, &at.step) == Some(signal)
    });
    let Some(at) = at else {
        return Err(Error::other(format!(
            "task {task_id} is not awaiting signal {signal:?}: it is {} at {}",
            task.status,
            StepAt::of(&task)
                .map(|at| at.to_string())
                .unwrap_or_else(|| "no step".to_string())
        )));
    };

    let check = submit_check(
        conn,
        task_id,
        &Submission {
            conclusion,
            author: Some(author.to_string()),
            body: note.clone(),
            at: Some(at.clone()),
        },
    )?;

    let outcome = match conclusion {
        Conclusion::Pass => Outcome::Pass,
        Conclusion::Fail => Outcome::Reject,
    };
    let report = StepReport::verdict(
        outcome,
        match &note {
            Some(note) => format!("signal {signal} said {}: {note}", conclusion.as_str()),
            None => format!("signal {signal} said {}", conclusion.as_str()),
        },
    );
    let moved = finish_step(conn, policy, task_id, &at, &report)?;
    Ok((check, moved))
}

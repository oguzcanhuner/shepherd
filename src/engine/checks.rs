//! Checks: verdicts about specific commits.
//!
//! A check is how anything — a linter, a reviewing agent, a person — says
//! pass or fail about the exact commit it looked at. `shep` stamps the sha
//! itself, so a submitter can never pin a verdict to code it did not judge.

use super::flow::finish_step;
use super::policy::awaits_human;
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

/// `shep approve` / `shep reject` — the only things that resolve a handoff.
///
/// The verdict is written as a check first, because a person approving a change is
/// a verdict about a commit like any other — and it is what `integrate`
/// will insist on. Then the step is finished with it, which is the same path a
/// script's verdict takes.
pub fn settle_by_human(
    conn: &mut Connection,
    policy: &Policy,
    task_id: &str,
    conclusion: Conclusion,
    author: &str,
    note: Option<String>,
) -> Result<(Check, TransitionOutcome)> {
    let task = task::require(conn, task_id)?;
    // One answer for every way a task can fail to be yours: nowhere yet, somewhere
    // else, or at a step whose pipeline never asks anyone.
    let at = StepAt::of(&task)
        .filter(|at| task.status == Status::Running && awaits_human(policy, &at.pipeline));
    let Some(at) = at else {
        return Err(Error::other(format!(
            "task {task_id} is not waiting for you: it is {} at {}",
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
            Some(note) => format!("{author} said {}: {note}", conclusion.as_str()),
            None => format!("{author} said {}", conclusion.as_str()),
        },
    );
    // `finish_step`'s guard re-checks the position inside its own transaction,
    // so a task that moved between the check and here bails rather than settles.
    let moved = finish_step(conn, policy, task_id, &at, &report)?;
    Ok((check, moved))
}

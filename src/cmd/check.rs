use crate::db::check::Conclusion;
use crate::db::{self, check};
use crate::engine::{self, StepAt, Submission};
use anyhow::{Result, bail};
use std::io::Read;
use std::path::Path;

/// `shep check submit --pass|--fail` — a verdict plus evidence about a commit,
/// with the body on stdin (PLAN §7.3).
///
/// The submitter never supplies the sha: `shep` stamps it from `git rev-parse
/// HEAD` in the worktree, so a stale check is an impossible state rather than an
/// agent-behaviour bug.
pub fn submit(
    db_path: &Path,
    pass: bool,
    fail: bool,
    author: Option<String>,
    task: Option<String>,
) -> Result<()> {
    let conclusion = match (pass, fail) {
        (true, false) => Conclusion::Pass,
        (false, true) => Conclusion::Fail,
        _ => bail!("say which: shep check submit --pass, or --fail"),
    };

    let mut conn = db::open(db_path)?;
    let task_id = super::task_id(&conn, task)?;

    let submission = Submission {
        conclusion,
        author,
        body: read_body()?,
        at: position_from_env(),
    };
    let written = engine::submit_check(&mut conn, &task_id, &submission)?;

    // stdout is the id and nothing else, so `C=$(shep check submit --pass)` works.
    println!("{}", written.id);
    Ok(())
}

/// `shep read c-7` — one addressed artefact.
pub fn read(db_path: &Path, id: &str) -> Result<()> {
    let conn = db::open_existing(db_path)?;
    let Some(found) = check::get(&conn, id)? else {
        bail!("no such check: {id}");
    };
    println!(
        "{} {} by {} on {} ({}/{} round {})",
        found.id,
        found.conclusion.as_str(),
        found.author,
        found.sha,
        found.pipeline.as_deref().unwrap_or("-"),
        found.step.as_deref().unwrap_or("-"),
        found.round.unwrap_or(0),
    );
    if let Some(body) = &found.body {
        println!("\n{body}");
    }
    Ok(())
}

/// The body, from stdin.
///
/// Only when stdin is not a terminal: an agent that types `shep check submit
/// --pass` at a prompt must get a check, not a process waiting for EOF.
fn read_body() -> Result<Option<String>> {
    if unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
        return Ok(None);
    }
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    let body = buf.trim();
    Ok(if body.is_empty() {
        None
    } else {
        Some(body.to_string())
    })
}

/// Where the caller says it is (PLAN §7.3). A step script has all three; an
/// agent's pane has none of them, and then the task's own position is used —
/// which is the position being awaited, and the only one that could be right.
fn position_from_env() -> Option<StepAt> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let pipeline = var("SHEP_PIPELINE")?;
    let step = var("SHEP_STEP")?;
    let round = var("SHEP_ROUND")?.parse().ok()?;
    Some(StepAt::new(pipeline, step, round))
}

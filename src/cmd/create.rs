use crate::config::Policy;
use crate::db::{self, task::NewTask};
use crate::engine;
use crate::supervisor;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// `shep create --type feature "..."` — INSERT task + event, status = queued.
/// The supervisor picks it up on its next poll.
pub fn run(
    db_path: &Path,
    kind: &str,
    repo: Option<PathBuf>,
    brief_words: &[String],
) -> Result<()> {
    let brief = brief_words.join(" ").trim().to_string();
    if brief.is_empty() {
        bail!("a task needs a brief: shep create --type {kind} \"what to do\"");
    }

    let repo = super::repo_root(repo)?;

    // The type has to exist before the task does: an agent that guessed wrong
    // should be told the menu, not have a task queued that can never run.
    let policy = Policy::load(&repo)?;
    policy.task_type(kind)?;

    let mut conn = db::open(db_path)?;
    let task = engine::create_task(
        &mut conn,
        NewTask {
            brief,
            kind: kind.to_string(),
            repo: repo.to_string_lossy().into_owned(),
        },
    )?;

    // stdout is the id and nothing else, so `TASK=$(shep create ...)` works.
    println!("{}", task.id);

    // A task queued into a store nothing is driving would sit there silently,
    // which reads as "shepherd is broken". Say so on stderr — and inside a
    // Herdr context, fix it.
    if !supervisor::health(&conn)?.is_healthy() {
        if supervisor::may_revive() && supervisor::revive(&conn, db_path)? {
            eprintln!("note: the supervisor was down; started one — {} will run shortly", task.id);
        } else {
            eprintln!(
                "warning: no supervisor is running, so {} will not start. \
                 Restart Herdr, or run `shep supervise` yourself.",
                task.id
            );
        }
    }
    Ok(())
}

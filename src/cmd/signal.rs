use crate::db::check::Conclusion;
use crate::engine::{self, TransitionOutcome};
use crate::{config::Policy, db, db::task};
use anyhow::{Result, bail};
use std::io::Read;
use std::path::Path;

/// `shep signal <task> --name <sig> --pass|--fail` — resolve a step that is
/// awaiting a named signal. Anything outside shepherd (CI, a webhook, a script)
/// redeems a deferred step this way; `shep approve` / `shep reject` are the
/// same thing for the built-in `human` signal.
pub fn run(
    db_path: &Path,
    name: &str,
    pass: bool,
    fail: bool,
    task: Option<String>,
    author: Option<String>,
    note: Option<String>,
) -> Result<()> {
    let conclusion = match (pass, fail) {
        (true, false) => Conclusion::Pass,
        (false, true) => Conclusion::Fail,
        _ => bail!("say which way: shep signal --name {name} --pass  or  --fail"),
    };

    let mut conn = db::open(db_path)?;
    let task_id = super::task_id(&conn, task)?;
    let row = task::require(&conn, &task_id)?;
    let policy = Policy::load(Path::new(&row.repo))?;

    let author = author.unwrap_or_else(|| name.to_string());
    let note = note.or(read_stdin()?);
    let (check, moved) =
        engine::settle_by_signal(&mut conn, &policy, &task_id, name, conclusion, &author, note)?;

    match moved {
        TransitionOutcome::Applied(applied) => {
            let now = &applied.task;
            let where_now = match (&now.pipeline, &now.step) {
                (Some(p), Some(s)) => format!("{p}/{s}"),
                _ => now.status.to_string(),
            };
            println!(
                "{task_id} {} on signal {name} as {} ({where_now})",
                conclusion.as_str(),
                check.id
            );
            Ok(())
        }
        // The check is written either way: it was true when the signal fired.
        TransitionOutcome::Bailed(reason) => bail!("{task_id} recorded {}: {reason}", check.id),
    }
}

/// A longer note, when piped. Never blocks on a tty.
fn read_stdin() -> Result<Option<String>> {
    if unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
        return Ok(None);
    }
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    let body = buf.trim();
    Ok((!body.is_empty()).then(|| body.to_string()))
}

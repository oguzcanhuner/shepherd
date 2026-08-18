use crate::db::{self, task::NewTask};
use crate::engine;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// `shep create --type feature "..."` — INSERT task + event, status = queued.
/// The supervisor picks it up on its next poll (PLAN §7.4).
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

    let repo = match repo {
        Some(p) => canonical(&p)?,
        None => repo_root()?,
    };

    // M3 validates --type against the repo's .shep/config.toml and prints the
    // menu of types on a miss. Until then the type is stored as given.
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
    Ok(())
}

fn canonical(path: &Path) -> Result<PathBuf> {
    std::fs::canonicalize(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

/// Config lives per repo root (PLAN §4), so that is what a task records.
fn repo_root() -> Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output();
    if let Some(out) = out.ok().filter(|o| o.status.success()) {
        let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !root.is_empty() {
            return Ok(PathBuf::from(root));
        }
    }
    Ok(std::env::current_dir()?)
}

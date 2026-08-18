//! The little bit of git the engine does itself.
//!
//! Worktree creation belongs to a step script, because `herdr worktree create`
//! makes the workspace at the same time and that is Herdr's business.
//! What is left here is what the engine has to know for itself: which commit a
//! check is a verdict about.

use crate::{Error, Result};
use std::path::Path;
use std::process::Command;

/// `git rev-parse HEAD` in a directory.
///
/// This is how `sha` gets onto a `check_run`: the submitter never supplies it,
/// or a stale check becomes an agent-behaviour bug instead of an impossible
/// state.
pub fn head_sha(dir: &Path) -> Result<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .map_err(|e| Error::other(format!("could not run git in {}: {e}", dir.display())))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Error::other(format!(
            "git rev-parse HEAD failed in {}: {}",
            dir.display(),
            stderr.trim()
        )));
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() {
        return Err(Error::other(format!(
            "git rev-parse HEAD said nothing in {} — no commits yet?",
            dir.display()
        )));
    }
    Ok(sha)
}

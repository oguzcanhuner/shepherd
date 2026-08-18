use crate::{db, logging, paths, supervisor};
use anyhow::{Context, Result};
use std::path::Path;
use std::time::Duration;

/// `shep supervise` — the daemon. Started by Herdr's `[[startup]]` (PLAN §4).
pub fn run(db_path: &Path, poll_ms: u64, ticks: Option<u64>) -> Result<()> {
    let log = paths::log_path_for(db_path);
    logging::init_file(&log, "info").with_context(|| format!("opening log {}", log.display()))?;

    let mut conn = db::open(db_path)?;
    let poll = Duration::from_millis(poll_ms.max(1));
    supervisor::run(&mut conn, poll, ticks)?;
    Ok(())
}

use crate::db::{self, meta};
use anyhow::Result;
use std::path::Path;

/// Pause and resume are a flag row the supervisor reads each tick.
pub fn run(db_path: &Path, paused: bool) -> Result<()> {
    let conn = db::open(db_path)?;
    let was = meta::is_paused(&conn)?;
    meta::set_paused(&conn, paused)?;
    match (was, paused) {
        (false, true) => println!("paused — the supervisor will start nothing new"),
        (true, false) => println!("resumed"),
        (true, true) => println!("already paused"),
        (false, false) => println!("not paused"),
    }
    Ok(())
}

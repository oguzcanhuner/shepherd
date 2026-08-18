//! The `pane_task` table: which Herdr pane belongs to which task.
//!
//! It earns its keep twice (PLAN §6): it makes a Herdr event attributable to a
//! task, and it lets a bare `shep context` resolve its own task from
//! `$HERDR_PANE_ID`.

use crate::Result;
use rusqlite::Connection;

/// Bind a pane to a task. A pane hosts one task at a time, so re-binding a pane
/// replaces what was there.
pub fn bind(conn: &Connection, pane_id: &str, task_id: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO pane_task (pane_id, task_id) VALUES (?1, ?2) \
         ON CONFLICT(pane_id) DO UPDATE SET task_id = excluded.task_id",
        rusqlite::params![pane_id, task_id],
    )?;
    Ok(())
}

pub fn unbind(conn: &Connection, pane_id: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM pane_task WHERE pane_id = ?1", [pane_id])? > 0)
}

/// Which task owns this pane, if any.
pub fn task_for(conn: &Connection, pane_id: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare_cached("SELECT task_id FROM pane_task WHERE pane_id = ?1")?;
    match stmt.query_one([pane_id], |r| r.get(0)) {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Which pane a task is bound to. Whether a task has a pane is what tells
/// orphan recovery apart from an agent still working (PLAN §11).
pub fn for_task(conn: &Connection, task_id: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare_cached("SELECT pane_id FROM pane_task WHERE task_id = ?1")?;
    match stmt.query_one([task_id], |r| r.get(0)) {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

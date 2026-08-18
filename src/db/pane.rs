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

/// Which pane a task is working in: the one bound most recently, since a task
/// outlives any one of its panes — `implement` gets an agent pane and `handoff`
/// will get a diff pane of its own.
pub fn for_task(conn: &Connection, task_id: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT pane_id FROM pane_task WHERE task_id = ?1 ORDER BY rowid DESC LIMIT 1",
    )?;
    match stmt.query_one([task_id], |r| r.get(0)) {
        Ok(id) => Ok(Some(id)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Every pane bound to a task, oldest first.
pub fn all_for_task(conn: &Connection, task_id: &str) -> Result<Vec<String>> {
    let mut stmt =
        conn.prepare_cached("SELECT pane_id FROM pane_task WHERE task_id = ?1 ORDER BY rowid")?;
    let rows = stmt.query_map([task_id], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The last agent status seen in a pane.
///
/// Kept because `pane_agent_status_changed` has no previous-status field
/// (herdr-findings §5.2): without this, "the agent finished" is indistinguishable
/// from "the agent is ready for its first prompt", and every deferred step would
/// resolve the moment it started.
pub fn last_status(conn: &Connection, pane_id: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare_cached("SELECT status FROM pane_agent WHERE pane_id = ?1")?;
    match stmt.query_one([pane_id], |r| r.get(0)) {
        Ok(status) => Ok(Some(status)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn record_status(conn: &Connection, pane_id: &str, status: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO pane_agent (pane_id, status, updated) VALUES (?1, ?2, ?3) \
         ON CONFLICT(pane_id) DO UPDATE SET status = excluded.status, updated = excluded.updated",
        rusqlite::params![pane_id, status, super::now()],
    )?;
    Ok(())
}

/// A pane that has gone away has no agent status. Pane ids are never reused
/// (herdr-findings §6), so this row could only ever go stale.
pub fn forget_status(conn: &Connection, pane_id: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM pane_agent WHERE pane_id = ?1", [pane_id])? > 0)
}

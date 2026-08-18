//! The `event` table: an append-only audit trail. Nothing reads it to make a
//! decision (PLAN §1), so its only jobs are `shep trace` and knowing what
//! happened.
//!
//! Event names are fixed and few. A step called `lint` finishing emits
//! `task.step_finished {step: "lint", ...}`, never `lint.finished` — otherwise
//! editing config mints new protocol (PLAN §6).

use crate::Result;
use rusqlite::Connection;

pub mod names {
    pub const TASK_CREATED: &str = "task.created";
    pub const TASK_STEP_STARTED: &str = "task.step_started";
    pub const TASK_STEP_FINISHED: &str = "task.step_finished";
    /// A step returned `started`: the answer comes later, per the pipeline's await.
    pub const TASK_STEP_AWAITING: &str = "task.step_awaiting";
    pub const TASK_PIPELINE_STARTED: &str = "task.pipeline_started";
    pub const TASK_PIPELINE_FINISHED: &str = "task.pipeline_finished";
    /// A pane (and the worktree it works in) is now this task's.
    pub const TASK_PANE_BOUND: &str = "task.pane_bound";
    /// A verdict about a commit was written to `check_run`.
    pub const TASK_CHECK_SUBMITTED: &str = "task.check_submitted";
    pub const TASK_PARKED: &str = "task.parked";
    pub const TASK_RESUMED: &str = "task.resumed";
    pub const TASK_CANCELLED: &str = "task.cancelled";
    pub const TASK_FINISHED: &str = "task.finished";
    pub const SUPERVISOR_STARTED: &str = "supervisor.started";
    pub const SUPERVISOR_STOPPED: &str = "supervisor.stopped";
}

/// An event to be written in the same transaction as the state change it records.
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub kind: String,
    pub task_id: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub caused_by: Option<i64>,
}

impl NewEvent {
    pub fn new(kind: impl Into<String>) -> Self {
        NewEvent {
            kind: kind.into(),
            task_id: None,
            payload: None,
            caused_by: None,
        }
    }

    pub fn for_task(kind: impl Into<String>, task_id: impl Into<String>) -> Self {
        NewEvent::new(kind).task(task_id)
    }

    pub fn task(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    pub fn payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = Some(payload);
        self
    }

    /// The seq that led here, for `shep trace`.
    pub fn caused_by(mut self, seq: i64) -> Self {
        self.caused_by = Some(seq);
        self
    }
}

#[derive(Debug, Clone)]
pub struct Event {
    pub seq: i64,
    pub ts: i64,
    pub kind: String,
    pub task_id: Option<String>,
    pub payload: Option<serde_json::Value>,
    pub caused_by: Option<i64>,
}

/// Append one event, returning its seq — the only ordering that matters.
pub fn append(conn: &Connection, event: &NewEvent) -> Result<i64> {
    let payload = match &event.payload {
        Some(v) => Some(serde_json::to_string(v)?),
        None => None,
    };
    conn.execute(
        "INSERT INTO event (ts, type, task_id, payload, caused_by) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            super::now(),
            event.kind,
            event.task_id,
            payload,
            event.caused_by
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

const COLUMNS: &str = "seq, ts, type, task_id, payload, caused_by";

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let payload: Option<String> = row.get("payload")?;
    Ok(Event {
        seq: row.get("seq")?,
        ts: row.get("ts")?,
        kind: row.get("type")?,
        task_id: row.get("task_id")?,
        // A payload we can't parse is worth showing raw rather than failing a read.
        payload: payload.and_then(|s| serde_json::from_str(&s).ok()),
        caused_by: row.get("caused_by")?,
    })
}

pub fn for_task(conn: &Connection, task_id: &str) -> Result<Vec<Event>> {
    let sql = format!("SELECT {COLUMNS} FROM event WHERE task_id = ?1 ORDER BY seq ASC");
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([task_id], from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn recent(conn: &Connection, limit: i64) -> Result<Vec<Event>> {
    let sql = format!("SELECT {COLUMNS} FROM event ORDER BY seq DESC LIMIT ?1");
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([limit], from_row)?;
    let mut events = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    events.reverse();
    Ok(events)
}

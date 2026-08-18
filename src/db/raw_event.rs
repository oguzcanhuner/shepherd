//! The `raw_event` table: what Herdr said, as it said it.
//!
//! The hook's whole job is to append here. Interpretation — which
//! pane, which task, whether this is the `working` → `done` edge — happens later
//! in the supervisor, because the payload carries no previous status and a hook
//! that decided things would have to keep state of its own.

use crate::Result;
use rusqlite::Connection;

#[derive(Debug, Clone)]
pub struct RawEvent {
    pub seq: i64,
    pub ts: i64,
    pub body: String,
}

impl RawEvent {
    /// The dotted event name, if the body is the JSON Herdr hands to a hook.
    /// Herdr underscores the name inside the payload and dots it in the
    /// environment; this reports what the payload says, dotted.
    pub fn kind(&self) -> Option<String> {
        let parsed: serde_json::Value = serde_json::from_str(&self.body).ok()?;
        let raw = parsed.get("event")?.as_str()?;
        Some(raw.replacen('_', ".", 1))
    }

    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.body).ok()
    }
}

/// Append one event, returning its seq.
pub fn append(conn: &Connection, body: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO raw_event (ts, body) VALUES (?1, ?2)",
        rusqlite::params![super::now(), body],
    )?;
    Ok(conn.last_insert_rowid())
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawEvent> {
    Ok(RawEvent {
        seq: row.get("seq")?,
        ts: row.get("ts")?,
        body: row.get("body")?,
    })
}

/// Newest last, so reading it top to bottom reads chronologically.
pub fn recent(conn: &Connection, limit: i64) -> Result<Vec<RawEvent>> {
    let mut stmt =
        conn.prepare_cached("SELECT seq, ts, body FROM raw_event ORDER BY seq DESC LIMIT ?1")?;
    let rows = stmt.query_map([limit], from_row)?;
    let mut events = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    events.reverse();
    Ok(events)
}

/// Everything after `cursor`, oldest first — how the supervisor will consume
/// these once deferred steps land in M5.
pub fn since(conn: &Connection, cursor: i64, limit: i64) -> Result<Vec<RawEvent>> {
    let mut stmt = conn.prepare_cached(
        "SELECT seq, ts, body FROM raw_event WHERE seq > ?1 ORDER BY seq ASC LIMIT ?2",
    )?;
    let rows = stmt.query_map([cursor, limit], from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_one("SELECT COUNT(*) FROM raw_event", [], |r| r.get(0))?)
}

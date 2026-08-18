//! The `meta` table: small facts that are not tasks. The supervisor heartbeat
//! and the pause flag live here — `shep pause` writes a row, the supervisor
//! reads it each tick.

use crate::{Error, Result};
use rusqlite::Connection;

pub const HEARTBEAT: &str = "supervisor.heartbeat";
pub const PAUSED: &str = "paused";
/// How far the supervisor has read into `raw_event`. What Herdr said is a log,
/// and this is the only reader's place in it.
pub const RAW_CURSOR: &str = "raw_event.cursor";

pub fn get(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare_cached("SELECT value FROM meta WHERE key = ?1")?;
    match stmt.query_one([key], |r| r.get(0)) {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, key: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM meta WHERE key = ?1", [key])? > 0)
}

pub fn get_json<T: serde::de::DeserializeOwned>(conn: &Connection, key: &str) -> Result<Option<T>> {
    match get(conn, key)? {
        None => Ok(None),
        Some(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| Error::corrupt("meta value", format!("{key}: {e}"))),
    }
}

pub fn set_json<T: serde::Serialize>(conn: &Connection, key: &str, value: &T) -> Result<()> {
    set(conn, key, &serde_json::to_string(value)?)
}

/// Where the supervisor has read up to in `raw_event`. Absent means "nothing
/// read yet", which is 0.
pub fn raw_cursor(conn: &Connection) -> Result<i64> {
    Ok(get_json(conn, RAW_CURSOR)?.unwrap_or(0))
}

pub fn set_raw_cursor(conn: &Connection, seq: i64) -> Result<()> {
    set_json(conn, RAW_CURSOR, &seq)
}

/// Is the supervisor being told to hold off? Read every tick.
pub fn is_paused(conn: &Connection) -> Result<bool> {
    Ok(get(conn, PAUSED)?.is_some_and(|v| v == "1"))
}

pub fn set_paused(conn: &Connection, paused: bool) -> Result<()> {
    if paused {
        set(conn, PAUSED, "1")
    } else {
        delete(conn, PAUSED).map(|_| ())
    }
}

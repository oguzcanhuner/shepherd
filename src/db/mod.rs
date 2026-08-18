//! The store. SQLite in WAL mode is the only IPC mechanism in the system
//! (PLAN §3, §7.4), so every connection sets the same pragmas.

pub mod event;
pub mod meta;
pub mod raw_event;
pub mod schema;
pub mod task;

use crate::{Error, Result};
use rusqlite::{Connection, TransactionBehavior};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// How long a writer waits for the write lock before giving up. Concurrent
/// writers must wait rather than see `SQLITE_BUSY` (PLAN §7.4).
pub const BUSY_TIMEOUT_MS: u32 = 10_000;

/// Seconds since the epoch. Every timestamp in the store is this.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Open the store, creating and migrating it if need be. For writers.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut conn = Connection::open(path)?;
    configure(&conn)?;
    schema::migrate(&mut conn, path)?;
    Ok(conn)
}

/// Open an existing store without creating one. For pure reads: `shep status`
/// on a machine that has never run anything should say so, not mint a database.
pub fn open_existing(path: &Path) -> Result<Connection> {
    if !path.exists() {
        return Err(Error::NoStore(path.to_path_buf()));
    }
    let conn = Connection::open(path)?;
    configure(&conn)?;
    schema::check_version(&conn, path)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    // WAL is persistent once set, but a fresh file needs it and setting it
    // again is free.
    let mode: String = conn.query_one("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(Error::other(format!(
            "could not put the store in WAL mode (got {mode:?})"
        )));
    }
    conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS as u64))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

/// Begin a write transaction. `BEGIN IMMEDIATE` takes the write lock up front so
/// that concurrent writers serialize instead of failing at commit (PLAN §6).
pub fn write_tx(conn: &mut Connection) -> Result<rusqlite::Transaction<'_>> {
    Ok(conn.transaction_with_behavior(TransactionBehavior::Immediate)?)
}

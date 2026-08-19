//! Schema and migrations. `user_version` is the version marker; each migration
//! runs in its own transaction together with the bump, so a half-applied
//! migration is not a state the store can be left in.

use crate::{Error, Result};
use rusqlite::Connection;
use std::path::Path;

/// Migration 1 — the data model, plus `meta`.
const M001_INITIAL: &str = r#"
CREATE TABLE task (
  id           TEXT PRIMARY KEY,
  brief        TEXT NOT NULL,
  type         TEXT NOT NULL,
  pipeline     TEXT,
  step         TEXT,
  round        INTEGER NOT NULL DEFAULT 0,
  status       TEXT NOT NULL,
  human_owned  INTEGER NOT NULL DEFAULT 0,
  repo         TEXT NOT NULL,
  worktree     TEXT,
  branch       TEXT,
  base         TEXT,
  workspace_id TEXT,
  created      INTEGER NOT NULL,
  updated      INTEGER NOT NULL
) STRICT;

CREATE INDEX task_status ON task(status);

CREATE TABLE check_run (
  id         TEXT PRIMARY KEY,
  task_id    TEXT NOT NULL REFERENCES task(id),
  pipeline   TEXT,
  step       TEXT,
  round      INTEGER,
  author     TEXT NOT NULL,
  sha        TEXT NOT NULL,
  conclusion TEXT NOT NULL,
  body       TEXT,
  created    INTEGER NOT NULL
) STRICT;

-- The lookup that resolves a deferred step: latest check for a
-- task + pipeline + step + round.
CREATE INDEX check_run_lookup ON check_run(task_id, pipeline, step, round, created);

CREATE TABLE event (
  seq       INTEGER PRIMARY KEY AUTOINCREMENT,
  ts        INTEGER NOT NULL,
  type      TEXT NOT NULL,
  task_id   TEXT,
  payload   TEXT,
  caused_by INTEGER
) STRICT;

CREATE INDEX event_task ON event(task_id, seq);

CREATE TABLE pane_task (
  pane_id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL REFERENCES task(id)
) STRICT;

CREATE INDEX pane_task_task ON pane_task(task_id);

-- What Herdr said, as it said it.
CREATE TABLE raw_event (
  seq  INTEGER PRIMARY KEY AUTOINCREMENT,
  ts   INTEGER NOT NULL,
  body TEXT NOT NULL
) STRICT;

-- Supervisor heartbeat and the pause flag: small facts that are not tasks.
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT;
"#;

/// Migration 2 — the last agent status seen in a pane.
///
/// `pane_agent_status_changed` carries no previous status (herdr-findings §5.2),
/// so the `working` → `done` edge can only be worked out against something we
/// kept ourselves. This is that: a projection of the status events in
/// `raw_event`, one row per pane a task is bound to.
const M002_PANE_AGENT: &str = r#"
CREATE TABLE pane_agent (
  pane_id TEXT PRIMARY KEY,
  status  TEXT NOT NULL,
  updated INTEGER NOT NULL
) STRICT;
"#;

/// Migration 3 — a task's plan: the ordered top-level pipelines it runs.
///
/// "What's next" moves out of the type's config and onto the row, so a pipeline
/// applied by hand (`shep run`) has somewhere to return to, and a task that runs
/// out of plan comes to rest rather than being stranded.
const M003_TASK_PLAN: &str = r#"
ALTER TABLE task ADD COLUMN plan TEXT NOT NULL DEFAULT '[]';
"#;

/// Migration 4 — the deadline an awaiting step must resolve by.
///
/// A deferred step with a `timeout` records when it must be resolved by; the
/// supervisor fires its `on_timeout` verdict once that passes. NULL means the
/// step waits without a deadline.
const M004_AWAIT_DEADLINE: &str = r#"
ALTER TABLE task ADD COLUMN await_deadline INTEGER;
"#;

const MIGRATIONS: &[&str] = &[
    M001_INITIAL,
    M002_PANE_AGENT,
    M003_TASK_PLAN,
    M004_AWAIT_DEADLINE,
];

/// The schema version this build writes.
pub fn latest_version() -> i64 {
    MIGRATIONS.len() as i64
}

pub fn version(conn: &Connection) -> Result<i64> {
    Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

/// Refuse to read a store written by a newer build rather than misinterpret it.
pub fn check_version(conn: &Connection, path: &Path) -> Result<i64> {
    let found = version(conn)?;
    if found > latest_version() {
        return Err(Error::SchemaTooNew {
            path: path.to_path_buf(),
            found,
            known: latest_version(),
        });
    }
    Ok(found)
}

pub fn migrate(conn: &mut Connection, path: &Path) -> Result<()> {
    let current = check_version(conn, path)?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let next = i as i64 + 1;
        let tx = crate::db::write_tx(conn)?;
        tx.execute_batch(sql)?;
        // pragma_update is not parameterised; next is derived from a literal list.
        tx.pragma_update(None, "user_version", next)?;
        tx.commit()?;
        tracing::info!(version = next, "applied migration");
    }
    Ok(())
}

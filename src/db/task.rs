//! The `task` table: the row that says where a unit of work has got to.

use crate::{Error, Result};
use rusqlite::{Connection, Row};
use std::fmt;

/// `status` values. Nothing else is legal in the column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Queued,
    Running,
    Parked,
    /// The task's plan is empty: it has run everything asked of it and now waits,
    /// idle, for a human or the orchestrator to apply another pipeline (with
    /// `shep run`) — or to leave it be. Non-terminal: a rested task can move again.
    Resting,
    Finished,
    Cancelled,
}

impl Status {
    pub const ALL: [Status; 6] = [
        Status::Queued,
        Status::Running,
        Status::Parked,
        Status::Resting,
        Status::Finished,
        Status::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Running => "running",
            Status::Parked => "parked",
            Status::Resting => "resting",
            Status::Finished => "finished",
            Status::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Result<Status> {
        Status::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| Error::corrupt("task.status", format!("unknown status {s:?}")))
    }

    /// A task in a terminal status will never move again on its own. Resting is
    /// *not* terminal: it is the idle state a task returns to between pipelines.
    pub fn is_terminal(self) -> bool {
        matches!(self, Status::Finished | Status::Cancelled)
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub brief: String,
    pub kind: String,
    pub pipeline: Option<String>,
    pub step: Option<String>,
    pub round: i64,
    pub status: Status,
    /// The top-level pipelines this task runs, in order — its remaining and
    /// completed plan. Seeded from the type at creation and extended by
    /// `shep run`. "What's next" is read from here, not from the type's config,
    /// so a pipeline applied by hand has somewhere to return to.
    pub plan: Vec<String>,
    /// When the current awaiting step must be resolved by, if it has a timeout.
    /// `None` means no deadline.
    pub await_deadline: Option<i64>,
    pub repo: String,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub base: Option<String>,
    pub workspace_id: Option<String>,
    pub created: i64,
    pub updated: i64,
}

/// What `shep create` supplies. Everything else is the engine's to decide.
#[derive(Debug, Clone)]
pub struct NewTask {
    pub brief: String,
    pub kind: String,
    pub repo: String,
    /// The type's pipelines, which seed the task's plan.
    pub plan: Vec<String>,
}

/// A set of fields to change. Applied to a row already re-read inside the
/// transaction, so the update is a whole-row write with no read-modify-write gap.
///
/// The outer `Option` is "leave alone"; the inner one, where present, is the
/// column's own nullability.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskPatch {
    pub status: Option<Status>,
    pub pipeline: Option<Option<String>>,
    pub step: Option<Option<String>>,
    pub round: Option<i64>,
    pub plan: Option<Vec<String>>,
    pub await_deadline: Option<Option<i64>>,
    pub worktree: Option<Option<String>>,
    pub branch: Option<Option<String>>,
    pub base: Option<Option<String>>,
    pub workspace_id: Option<Option<String>>,
}

impl TaskPatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn status(mut self, s: Status) -> Self {
        self.status = Some(s);
        self
    }

    pub fn pipeline(mut self, p: Option<impl Into<String>>) -> Self {
        self.pipeline = Some(p.map(Into::into));
        self
    }

    pub fn step(mut self, s: Option<impl Into<String>>) -> Self {
        self.step = Some(s.map(Into::into));
        self
    }

    pub fn round(mut self, r: i64) -> Self {
        self.round = Some(r);
        self
    }

    pub fn plan(mut self, plan: Vec<String>) -> Self {
        self.plan = Some(plan);
        self
    }

    pub fn await_deadline(mut self, deadline: Option<i64>) -> Self {
        self.await_deadline = Some(deadline);
        self
    }

    pub fn worktree(mut self, w: Option<impl Into<String>>) -> Self {
        self.worktree = Some(w.map(Into::into));
        self
    }

    pub fn branch(mut self, b: Option<impl Into<String>>) -> Self {
        self.branch = Some(b.map(Into::into));
        self
    }

    pub fn base(mut self, b: Option<impl Into<String>>) -> Self {
        self.base = Some(b.map(Into::into));
        self
    }

    pub fn workspace_id(mut self, w: Option<impl Into<String>>) -> Self {
        self.workspace_id = Some(w.map(Into::into));
        self
    }

    pub fn apply(&self, task: &mut Task) {
        macro_rules! set {
            ($field:ident) => {
                if let Some(v) = self.$field.clone() {
                    task.$field = v;
                }
            };
        }
        set!(status);
        set!(pipeline);
        set!(step);
        set!(round);
        set!(plan);
        set!(await_deadline);
        set!(worktree);
        set!(branch);
        set!(base);
        set!(workspace_id);
    }
}

const COLUMNS: &str = "id, brief, type, pipeline, step, round, status, \
                       plan, await_deadline, repo, worktree, branch, base, workspace_id, \
                       created, updated";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Task> {
    let status: String = row.get("status")?;
    let plan_json: String = row.get("plan")?;
    Ok(Task {
        id: row.get("id")?,
        brief: row.get("brief")?,
        kind: row.get("type")?,
        pipeline: row.get("pipeline")?,
        step: row.get("step")?,
        round: row.get("round")?,
        plan: serde_json::from_str(&plan_json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(Error::corrupt("task.plan", e.to_string())),
            )
        })?,
        await_deadline: row.get("await_deadline")?,
        // A bad status is corruption, not a query error; surface it as a value we
        // can report rather than mapping it into rusqlite's error space.
        status: Status::parse(&status).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        repo: row.get("repo")?,
        worktree: row.get("worktree")?,
        branch: row.get("branch")?,
        base: row.get("base")?,
        workspace_id: row.get("workspace_id")?,
        created: row.get("created")?,
        updated: row.get("updated")?,
    })
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Task>> {
    let sql = format!("SELECT {COLUMNS} FROM task WHERE id = ?1");
    let mut stmt = conn.prepare_cached(&sql)?;
    match stmt.query_one([id], from_row) {
        Ok(t) => Ok(Some(t)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn require(conn: &Connection, id: &str) -> Result<Task> {
    get(conn, id)?.ok_or_else(|| Error::TaskNotFound(id.to_string()))
}

/// Every task, newest first.
pub fn list(conn: &Connection) -> Result<Vec<Task>> {
    let sql = format!("SELECT {COLUMNS} FROM task ORDER BY created DESC, id DESC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Tasks in one status, oldest first — the order the supervisor should pick work up in.
pub fn list_by_status(conn: &Connection, status: Status) -> Result<Vec<Task>> {
    let sql = format!("SELECT {COLUMNS} FROM task WHERE status = ?1 ORDER BY created ASC, id ASC");
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([status.as_str()], from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The tasks living in a Herdr workspace. `workspace.closed` carries no pane id,
/// only a workspace, which is the whole reason `workspace_id` is on the row.
pub fn by_workspace(conn: &Connection, workspace_id: &str) -> Result<Vec<Task>> {
    let sql = format!("SELECT {COLUMNS} FROM task WHERE workspace_id = ?1 ORDER BY created ASC");
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([workspace_id], from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn counts_by_status(conn: &Connection) -> Result<Vec<(Status, i64)>> {
    let mut stmt = conn.prepare("SELECT status, COUNT(*) FROM task GROUP BY status")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (status, n) = row?;
        out.push((Status::parse(&status)?, n));
    }
    Ok(out)
}

/// Allocate the next task id. Called inside the write transaction, so the read
/// and the insert cannot interleave with another writer.
pub fn next_id(conn: &Connection) -> Result<String> {
    let n: i64 = conn.query_one(
        "SELECT IFNULL(MAX(CAST(SUBSTR(id, 3) AS INTEGER)), 0) + 1 \
         FROM task WHERE id GLOB 't-[0-9]*'",
        [],
        |r| r.get(0),
    )?;
    Ok(format!("t-{n}"))
}

pub fn insert(conn: &Connection, task: &Task) -> Result<()> {
    conn.execute(
        "INSERT INTO task (id, brief, type, pipeline, step, round, status, \
                           plan, await_deadline, repo, worktree, branch, base, workspace_id, \
                           created, updated) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        rusqlite::params![
            task.id,
            task.brief,
            task.kind,
            task.pipeline,
            task.step,
            task.round,
            task.status.as_str(),
            serde_json::to_string(&task.plan).unwrap_or_else(|_| "[]".to_string()),
            task.await_deadline,
            task.repo,
            task.worktree,
            task.branch,
            task.base,
            task.workspace_id,
            task.created,
            task.updated,
        ],
    )?;
    Ok(())
}

/// Whole-row update. Only ever called with a row re-read in the same transaction.
pub fn update(conn: &Connection, task: &Task) -> Result<()> {
    let changed = conn.execute(
        "UPDATE task SET brief = ?2, type = ?3, pipeline = ?4, step = ?5, round = ?6, \
                         status = ?7, plan = ?8, await_deadline = ?9, \
                         repo = ?10, worktree = ?11, branch = ?12, base = ?13, \
                         workspace_id = ?14, updated = ?15 \
         WHERE id = ?1",
        rusqlite::params![
            task.id,
            task.brief,
            task.kind,
            task.pipeline,
            task.step,
            task.round,
            task.status.as_str(),
            serde_json::to_string(&task.plan).unwrap_or_else(|_| "[]".to_string()),
            task.await_deadline,
            task.repo,
            task.worktree,
            task.branch,
            task.base,
            task.workspace_id,
            task.updated,
        ],
    )?;
    if changed != 1 {
        return Err(Error::TaskNotFound(task.id.clone()));
    }
    Ok(())
}

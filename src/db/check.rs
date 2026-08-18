//! The `check_run` table: a verdict plus evidence about a specific commit.
//!
//! Written by linters, test runs, reviewing agents and humans alike.
//! `sha` is load-bearing: a check is a verdict about a particular state of the
//! code, so `integrate` must refuse to pass on a check whose sha isn't head, or a
//! stale pass waves a bad merge through.

use crate::{Error, Result};
use rusqlite::Connection;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conclusion {
    Pass,
    Fail,
}

impl Conclusion {
    pub fn as_str(self) -> &'static str {
        match self {
            Conclusion::Pass => "pass",
            Conclusion::Fail => "fail",
        }
    }

    pub fn parse(s: &str) -> Result<Conclusion> {
        match s {
            "pass" => Ok(Conclusion::Pass),
            "fail" => Ok(Conclusion::Fail),
            other => Err(Error::corrupt(
                "check_run.conclusion",
                format!("unknown conclusion {other:?}"),
            )),
        }
    }
}

impl fmt::Display for Conclusion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub id: String,
    pub task_id: String,
    pub pipeline: Option<String>,
    pub step: Option<String>,
    pub round: Option<i64>,
    pub author: String,
    pub sha: String,
    pub conclusion: Conclusion,
    pub body: Option<String>,
    pub created: i64,
}

/// What a submitter supplies. Notably not the sha: `shep` stamps that itself, or
/// a stale check becomes an agent-behaviour bug instead of an impossible state
///.
#[derive(Debug, Clone)]
pub struct NewCheck {
    pub task_id: String,
    pub pipeline: Option<String>,
    pub step: Option<String>,
    pub round: Option<i64>,
    pub author: String,
    pub sha: String,
    pub conclusion: Conclusion,
    pub body: Option<String>,
}

const COLUMNS: &str = "id, task_id, pipeline, step, round, author, sha, conclusion, body, created";

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Check> {
    let conclusion: String = row.get("conclusion")?;
    Ok(Check {
        id: row.get("id")?,
        task_id: row.get("task_id")?,
        pipeline: row.get("pipeline")?,
        step: row.get("step")?,
        round: row.get("round")?,
        author: row.get("author")?,
        sha: row.get("sha")?,
        conclusion: Conclusion::parse(&conclusion).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        body: row.get("body")?,
        created: row.get("created")?,
    })
}

/// Allocate the next check id. Called inside the write transaction.
pub fn next_id(conn: &Connection) -> Result<String> {
    let n: i64 = conn.query_one(
        "SELECT IFNULL(MAX(CAST(SUBSTR(id, 3) AS INTEGER)), 0) + 1 \
         FROM check_run WHERE id GLOB 'c-[0-9]*'",
        [],
        |r| r.get(0),
    )?;
    Ok(format!("c-{n}"))
}

pub fn insert(conn: &Connection, new: &NewCheck) -> Result<Check> {
    let check = Check {
        id: next_id(conn)?,
        task_id: new.task_id.clone(),
        pipeline: new.pipeline.clone(),
        step: new.step.clone(),
        round: new.round,
        author: new.author.clone(),
        sha: new.sha.clone(),
        conclusion: new.conclusion,
        body: new.body.clone(),
        created: super::now(),
    };
    conn.execute(
        "INSERT INTO check_run (id, task_id, pipeline, step, round, author, sha, conclusion, \
                                body, created) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            check.id,
            check.task_id,
            check.pipeline,
            check.step,
            check.round,
            check.author,
            check.sha,
            check.conclusion.as_str(),
            check.body,
            check.created,
        ],
    )?;
    Ok(check)
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Check>> {
    let sql = format!("SELECT {COLUMNS} FROM check_run WHERE id = ?1");
    let mut stmt = conn.prepare_cached(&sql)?;
    match stmt.query_one([id], from_row) {
        Ok(check) => Ok(Some(check)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Every check on a task, newest last.
pub fn for_task(conn: &Connection, task_id: &str) -> Result<Vec<Check>> {
    let sql = format!("SELECT {COLUMNS} FROM check_run WHERE task_id = ?1 ORDER BY created, id");
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map([task_id], from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The check that resolves a deferred step: the latest for this task, pipeline,
/// step and round. No check means the step errored.
pub fn latest_for_step(
    conn: &Connection,
    task_id: &str,
    pipeline: &str,
    step: &str,
    round: i64,
) -> Result<Option<Check>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM check_run \
         WHERE task_id = ?1 AND pipeline = ?2 AND step = ?3 AND round = ?4 \
         ORDER BY created DESC, id DESC LIMIT 1"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    match stmt.query_one(rusqlite::params![task_id, pipeline, step, round], from_row) {
        Ok(check) => Ok(Some(check)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

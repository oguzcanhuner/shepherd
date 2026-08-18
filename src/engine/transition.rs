//! `transition()` — the only way a task row changes.
//!
//! The shape is: take the write lock, re-read the row, let the caller decide
//! against what is actually there, then write the state change and its events in
//! the same commit. The caller's decision function is where the guard lives: if
//! the row has moved since the caller looked at it, the function returns
//! [`Decision::Bail`] and nothing is written.
//!
//! This is why there is no optimistic version column and no lock file. SQLite
//! serializes the transactions; re-reading inside one makes the read-modify-write
//! atomic (PLAN §6).

use crate::db::{self, event::NewEvent, task};
use crate::{Error, Result};
use rusqlite::Connection;

/// What the caller decided to do about the row it was just shown.
#[derive(Debug, Clone)]
pub enum Decision {
    /// Change these fields and record these events.
    Apply {
        patch: task::TaskPatch,
        events: Vec<NewEvent>,
    },
    /// The row is not what the caller expected, or the work is already done.
    /// Nothing is written and the reason is returned.
    Bail(String),
}

impl Decision {
    pub fn apply(patch: task::TaskPatch) -> Self {
        Decision::Apply {
            patch,
            events: Vec::new(),
        }
    }

    pub fn with_event(mut self, event: NewEvent) -> Self {
        if let Decision::Apply { events, .. } = &mut self {
            events.push(event);
        }
        self
    }

    pub fn bail(reason: impl Into<String>) -> Self {
        Decision::Bail(reason.into())
    }
}

/// A committed transition.
#[derive(Debug, Clone)]
pub struct Applied {
    /// The row as it now stands.
    pub task: task::Task,
    /// The row as it was before, for reporting what moved.
    pub previous: task::Task,
    /// Seqs of the events written alongside, in order.
    pub events: Vec<i64>,
}

// `Applied` carries two whole task rows, so the enum is lopsided. It is
// returned once per transition and never in a hot loop, so a Box here would buy
// nothing but an extra deref at every use site.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Outcome {
    Applied(Applied),
    /// The decision function declined; the store is untouched.
    Bailed(String),
}

impl Outcome {
    pub fn applied(&self) -> Option<&Applied> {
        match self {
            Outcome::Applied(a) => Some(a),
            Outcome::Bailed(_) => None,
        }
    }

    pub fn is_applied(&self) -> bool {
        matches!(self, Outcome::Applied(_))
    }
}

/// Run one state transition against `task_id`.
///
/// `decide` is called with the row as it exists inside the write transaction —
/// not as the caller last saw it. Returning `Err` from `decide` rolls back, so a
/// failed decision can never leave a half-written change or an orphan event.
pub fn transition<F>(conn: &mut Connection, task_id: &str, decide: F) -> Result<Outcome>
where
    F: FnOnce(&task::Task) -> Result<Decision>,
{
    let tx = db::write_tx(conn)?;
    let current =
        task::get(&tx, task_id)?.ok_or_else(|| Error::TaskNotFound(task_id.to_string()))?;

    let decision = decide(&current)?;
    let (patch, events) = match decision {
        Decision::Bail(reason) => {
            // Dropping the transaction rolls it back; there was nothing to write.
            drop(tx);
            tracing::debug!(task = %task_id, %reason, "transition bailed");
            return Ok(Outcome::Bailed(reason));
        }
        Decision::Apply { patch, events } => (patch, events),
    };

    let mut next = current.clone();
    patch.apply(&mut next);
    next.updated = db::now();
    task::update(&tx, &next)?;

    let mut seqs = Vec::with_capacity(events.len());
    for e in &events {
        // An event about this transition belongs to this task, whatever the
        // caller filled in.
        let e = if e.task_id.is_none() {
            e.clone().task(task_id)
        } else {
            e.clone()
        };
        seqs.push(db::event::append(&tx, &e)?);
    }

    tx.commit()?;
    Ok(Outcome::Applied(Applied {
        task: next,
        previous: current,
        events: seqs,
    }))
}

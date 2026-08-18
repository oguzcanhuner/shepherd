//! Resolving a deferred step (PLAN §7.2).
//!
//! A step that returned `started` made a promise, not an answer. What redeems it
//! is a Herdr event: the pane's agent stopping, the pane going away, or the
//! workspace being closed. The hook only appends to `raw_event` — deciding
//! happens here, because the payload carries no previous status and a hook that
//! kept state of its own would be a second source of truth (PLAN §M2).
//!
//! Two things make this safe to run every tick:
//!
//! - **A cursor, not a subscription.** `raw_event` is a log and `meta.raw_cursor`
//!   is this reader's place in it, so an event is acted on once and a supervisor
//!   that was down while an agent finished still sees it when it comes back.
//! - **The edge, not the level.** `agent_status_changed` has no previous-status
//!   field (herdr-findings §5.2), and `herdr agent start` returns *once the agent
//!   is ready for input* — which is a status change in its own right. Resolving on
//!   `done` alone would therefore resolve every deferred step the moment it
//!   started, so resolution needs a remembered `working` first.

use super::{StepAt, StepReport, finish_step, policy_for};
use crate::Outcome;
use crate::Result;
use crate::config::Await;
use crate::db::check::Conclusion;
use crate::db::raw_event::RawEvent;
use crate::db::task::Status;
use crate::db::{check, meta, pane, raw_event, task};
use rusqlite::Connection;

/// How many raw events one drain will look at. Bounded so a tick stays short;
/// the rest wait for the next one, which is 200ms away.
pub const BATCH: i64 = 200;

/// Herdr's agent statuses (herdr-findings §5.1). Anything unrecognised is
/// `Unknown`, which resolves nothing — `unknown` "does not prove completion",
/// and a status word this build has never heard of proves even less.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

impl AgentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "blocked",
            AgentStatus::Done => "done",
            AgentStatus::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> AgentStatus {
        match s {
            "idle" => AgentStatus::Idle,
            "working" => AgentStatus::Working,
            "blocked" => AgentStatus::Blocked,
            "done" => AgentStatus::Done,
            _ => AgentStatus::Unknown,
        }
    }

    /// The agent has stopped. `done` is the one to expect for unattended work;
    /// `idle` is the same underlying state once the tab has been seen, which is
    /// what you get if you happened to be looking at it (herdr-findings §5.1).
    pub fn is_stopped(self) -> bool {
        matches!(self, AgentStatus::Done | AgentStatus::Idle)
    }

    /// The agent was doing something, so stopping means something. `blocked`
    /// counts: an agent that hit an approval prompt and then went quiet has
    /// stopped working, whether or not it got anywhere.
    pub fn was_working(self) -> bool {
        matches!(self, AgentStatus::Working | AgentStatus::Blocked)
    }
}

/// What one Herdr event means for a deferred step.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Signal {
    /// An agent's status changed in a pane.
    Status { pane: String, status: AgentStatus },
    /// A pane went away, which ends whatever was running in it.
    PaneGone { pane: String, how: String },
    /// A workspace closed, taking its panes with it and firing no pane events at
    /// all (NOTES.md). The payload has no pane id, which is why tasks record
    /// their `workspace_id`.
    WorkspaceGone { workspace: String },
    /// Everything else. Herdr sends plenty we hooked for other milestones.
    Ignored,
}

/// Read one stored event. Nothing here fails: an event we cannot parse is one we
/// ignore, because the alternative is a malformed payload wedging the loop.
fn read(raw: &RawEvent) -> Signal {
    let Some(json) = raw.json() else {
        return Signal::Ignored;
    };
    let data = json.get("data").unwrap_or(&json);
    let field = |name: &str| data.get(name).and_then(|v| v.as_str()).map(str::to_string);
    match raw.kind().as_deref() {
        Some("pane.agent_status_changed") => match (field("pane_id"), field("agent_status")) {
            (Some(pane), Some(status)) => Signal::Status {
                pane,
                status: AgentStatus::parse(&status),
            },
            _ => Signal::Ignored,
        },
        // `pane.exited` lags by 20-25 seconds and sometimes never arrives at all
        // (NOTES.md), so it is a backstop rather than the trigger. It still has to
        // be honoured: an agent that quit outright never reports `done`.
        Some(kind @ ("pane.exited" | "pane.closed")) => match field("pane_id") {
            Some(pane) => Signal::PaneGone {
                pane,
                how: kind.to_string(),
            },
            None => Signal::Ignored,
        },
        Some("workspace.closed") => match field("workspace_id") {
            Some(workspace) => Signal::WorkspaceGone { workspace },
            None => Signal::Ignored,
        },
        _ => Signal::Ignored,
    }
}

/// What a drain did. Returned rather than logged so a tick can report it and a
/// test can assert on it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Drained {
    /// Raw events read this time.
    pub consumed: usize,
    /// Deferred steps that got their answer.
    pub resolved: usize,
    /// Where the cursor now stands.
    pub cursor: i64,
}

/// Read everything Herdr has said since last time, and resolve what it resolves.
pub fn drain(conn: &mut Connection, limit: i64) -> Result<Drained> {
    let mut cursor = meta::raw_cursor(conn)?;
    let events = raw_event::since(conn, cursor, limit)?;
    let mut drained = Drained {
        cursor,
        ..Default::default()
    };

    for raw in &events {
        let signal = read(raw);
        // Errors are logged and the cursor still moves. An event that cannot be
        // dealt with must not be read again forever, or one bad payload stops
        // every task in the store from ever finishing.
        if let Err(e) = act(conn, &signal, &mut drained) {
            tracing::error!(seq = raw.seq, "could not act on a Herdr event: {e}");
        }
        cursor = raw.seq;
        meta::set_raw_cursor(conn, cursor)?;
        drained.consumed += 1;
        drained.cursor = cursor;
    }

    Ok(drained)
}

fn act(conn: &mut Connection, signal: &Signal, drained: &mut Drained) -> Result<()> {
    match signal {
        Signal::Ignored => Ok(()),

        Signal::Status { pane, status } => {
            // Only panes a task is working in are worth remembering, and only a
            // task's own panes can resolve anything.
            let Some(task_id) = pane::task_for(conn, pane)? else {
                return Ok(());
            };
            let previous = pane::last_status(conn, pane)?.map(|s| AgentStatus::parse(&s));
            pane::record_status(conn, pane, status.as_str())?;

            let stopped_working = status.is_stopped() && previous.is_some_and(|p| p.was_working());
            if !stopped_working {
                tracing::debug!(
                    task = %task_id, %pane,
                    from = previous.map(AgentStatus::as_str).unwrap_or("-"),
                    to = status.as_str(),
                    "status change, not an ending"
                );
                return Ok(());
            }
            let cause = format!("its agent went {} in pane {pane}", status.as_str());
            if resolve_awaiting(conn, &task_id, &cause)? {
                drained.resolved += 1;
            }
            Ok(())
        }

        Signal::PaneGone { pane, how } => {
            let task_id = pane::task_for(conn, pane)?;
            pane::forget_status(conn, pane)?;
            let Some(task_id) = task_id else {
                return Ok(());
            };
            if resolve_awaiting(conn, &task_id, &format!("{how} for pane {pane}"))? {
                drained.resolved += 1;
            }
            Ok(())
        }

        Signal::WorkspaceGone { workspace } => {
            for task in task::by_workspace(conn, workspace)? {
                for pane in pane::all_for_task(conn, &task.id)? {
                    pane::forget_status(conn, &pane)?;
                }
                if resolve_awaiting(conn, &task.id, &format!("workspace {workspace} closed"))? {
                    drained.resolved += 1;
                }
            }
            Ok(())
        }
    }
}

/// Give a task's deferred step its answer, if that is what this task is waiting
/// for. Returns whether anything moved.
///
/// Most events reach here about a task that is not waiting on anything — you
/// typed at an agent whose pane is still bound while `review` runs, say — so
/// declining is the common case and must be quiet and harmless.
pub fn resolve_awaiting(conn: &mut Connection, task_id: &str, cause: &str) -> Result<bool> {
    let task = task::require(conn, task_id)?;
    if task.status != Status::Running {
        return Ok(false);
    }
    let Some(at) = StepAt::of(&task) else {
        return Ok(false);
    };

    // Without the policy there is no telling whether this step is even deferred,
    // and guessing either way is worse than saying so: parking a task whose step
    // is fine is as bad as advancing one that is not. A task whose policy will
    // not load parks the next time the supervisor tries to run something for it.
    let policy = match policy_for(&task) {
        Ok(policy) => policy,
        Err(e) => {
            tracing::warn!(task = %task_id, "cannot resolve {at}: {e}");
            return Ok(false);
        }
    };
    let awaits = policy
        .config
        .pipeline
        .get(&at.pipeline)
        .and_then(|p| p.await_on);
    match awaits {
        Some(Await::AgentStopped) => {}
        // Nothing resolves a handoff but `shep approve` / `shep reject`. Status
        // events for its pane are kept in `raw_event` and advance nothing
        // (PLAN §7.2) — the whole point is that you can talk to the agent
        // without the state machine moving under you.
        Some(Await::Human) => return Ok(false),
        None => return Ok(false),
    }
    if task.human_owned {
        tracing::debug!(task = %task_id, "muted: {cause}");
        return Ok(false);
    }

    // The verdict is a `check_run` row, not anything the agent told us directly.
    // No check means the step errored: an agent that stopped without leaving one
    // may have run out of turns, crashed, or been interrupted, and none of those
    // are a pass (PLAN §7.2).
    let check = check::latest_for_step(conn, task_id, &at.pipeline, &at.step, at.round)?;
    let report = match &check {
        Some(c) => StepReport::verdict(
            match c.conclusion {
                Conclusion::Pass => Outcome::Pass,
                Conclusion::Fail => Outcome::Reject,
            },
            format!(
                "{cause}; {} {} by {} (sha {})",
                c.id,
                c.conclusion.as_str(),
                c.author,
                short_sha(&c.sha)
            ),
        ),
        None => StepReport::verdict(
            Outcome::Error,
            format!("{cause}, leaving no check for {at} — nothing says whether it worked"),
        ),
    };

    tracing::info!(
        task = %task_id, step = %at.step, round = at.round,
        outcome = report.outcome.as_str(),
        "deferred step resolved: {cause}"
    );
    let outcome = finish_step(conn, &policy, task_id, &at, &report)?;
    if let super::TransitionOutcome::Bailed(reason) = &outcome {
        tracing::warn!(task = %task_id, "resolution discarded: {reason}");
        return Ok(false);
    }
    Ok(true)
}

fn short_sha(sha: &str) -> &str {
    let end = sha
        .char_indices()
        .nth(7)
        .map(|(i, _)| i)
        .unwrap_or(sha.len());
    &sha[..end]
}

/// The store's own record of a pane's agent status, for `shep get` and tests.
pub fn pane_status(conn: &Connection, pane_id: &str) -> Result<Option<AgentStatus>> {
    Ok(pane::last_status(conn, pane_id)?.map(|s| AgentStatus::parse(&s)))
}

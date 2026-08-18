//! The supervisor: heartbeat, pause flag, and the poll loop.
//!
//! There is no socket and no server. The supervisor's whole job is
//! to poll the store, spawn step scripts and block on them, so aliveness is a
//! row in `meta` rather than a connection you can dial.

use crate::config::Policy;
use crate::db::{self, event, meta, task};
use crate::engine::{self, Started};
use crate::{Error, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// Default poll interval. Task creation is picked up on the next tick, which is
/// why ~200ms is the number: fast enough that it does not feel deferred.
pub const DEFAULT_POLL: Duration = Duration::from_millis(200);

/// A heartbeat older than this means the supervisor is wedged, not merely busy.
pub const STALE_AFTER: i64 = 5;

/// How long a clean shutdown waits for in-flight steps to report.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Written at most once a second, so a status check never has to guess.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Heartbeat {
    pub pid: i32,
    /// When this supervisor started.
    pub started: i64,
    /// When it last said anything.
    pub beat: i64,
}

/// What `shep status` reports. Correct with the supervisor up, shut down
/// cleanly, or killed outright.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// No heartbeat row: never started, or stopped cleanly.
    Down,
    /// Beating, and the process is there.
    Running { heartbeat: Heartbeat, age: i64 },
    /// The process is there but has stopped beating.
    Stalled { heartbeat: Heartbeat, age: i64 },
    /// A heartbeat row left behind by a process that no longer exists.
    Dead { heartbeat: Heartbeat, age: i64 },
}

impl Health {
    pub fn label(&self) -> &'static str {
        match self {
            Health::Down => "down",
            Health::Running { .. } => "running",
            Health::Stalled { .. } => "stalled",
            Health::Dead { .. } => "dead",
        }
    }

    pub fn heartbeat(&self) -> Option<&Heartbeat> {
        match self {
            Health::Down => None,
            Health::Running { heartbeat, .. }
            | Health::Stalled { heartbeat, .. }
            | Health::Dead { heartbeat, .. } => Some(heartbeat),
        }
    }

    /// True only when something is actually driving the store forward.
    pub fn is_healthy(&self) -> bool {
        matches!(self, Health::Running { .. })
    }
}

pub fn read_heartbeat(conn: &Connection) -> Result<Option<Heartbeat>> {
    meta::get_json(conn, meta::HEARTBEAT)
}

pub fn health(conn: &Connection) -> Result<Health> {
    health_at(conn, db::now())
}

/// `now` is a parameter so the ageing rules can be tested without sleeping.
pub fn health_at(conn: &Connection, now: i64) -> Result<Health> {
    let Some(heartbeat) = read_heartbeat(conn)? else {
        return Ok(Health::Down);
    };
    // Clamp: a heartbeat from the future (clock skew) is not negative age.
    let age = (now - heartbeat.beat).max(0);
    Ok(if !pid_alive(heartbeat.pid) {
        Health::Dead { heartbeat, age }
    } else if age > STALE_AFTER {
        Health::Stalled { heartbeat, age }
    } else {
        Health::Running { heartbeat, age }
    })
}

/// Is this pid a live process? `kill(pid, 0)` succeeds, or fails with EPERM
/// because the process exists but belongs to someone else.
pub fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn beat(conn: &Connection, started: i64) -> Result<()> {
    let hb = Heartbeat {
        pid: std::process::id() as i32,
        started,
        beat: db::now(),
    };
    meta::set_json(conn, meta::HEARTBEAT, &hb)
}

/// What one tick did. Returned so tests can drive the loop one step at a time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tick {
    pub paused: bool,
    pub queued: usize,
    pub running: usize,
    /// Steps this tick started.
    pub started: usize,
    /// Steps this supervisor has in flight right now.
    pub inflight: usize,
    /// Herdr events read this tick.
    pub events: usize,
    /// Deferred steps this tick resolved.
    pub resolved: usize,
}

/// The steps this supervisor has running, one thread each.
///
/// One thread per in-flight step, blocking on `child.wait()`. At three or four
/// concurrent tasks that is cheaper and far simpler than an async runtime
///.
#[derive(Default)]
pub struct Inflight {
    threads: HashMap<String, JoinHandle<()>>,
}

impl Inflight {
    pub fn len(&self) -> usize {
        self.threads.len()
    }

    pub fn is_empty(&self) -> bool {
        self.threads.is_empty()
    }

    /// Collect finished threads, surfacing a panic rather than swallowing it.
    fn reap(&mut self) {
        let done: Vec<String> = self
            .threads
            .iter()
            .filter(|(_, handle)| handle.is_finished())
            .map(|(id, _)| id.clone())
            .collect();
        for id in done {
            if let Some(handle) = self.threads.remove(&id)
                && let Err(panic) = handle.join()
            {
                // A panicked step thread leaves the task `running` with no pane,
                // which is exactly what recovery re-queues.
                tracing::error!(task = %id, "step thread panicked: {panic:?}");
            }
        }
    }
}

/// One pass over the store: start what is ready, and notice what has finished.
pub fn tick(conn: &mut Connection, db_path: &Path, inflight: &mut Inflight) -> Result<Tick> {
    inflight.reap();

    if meta::is_paused(conn)? {
        return Ok(Tick {
            paused: true,
            inflight: inflight.len(),
            ..Default::default()
        });
    }

    // What Herdr has said since last time, before looking for work: a deferred
    // step resolved now leaves its task queued, and the same tick starts it.
    let drained = engine::drain(conn, engine::resolve::BATCH)?;

    let queued = task::list_by_status(conn, task::Status::Queued)?;
    let mut started = 0;
    for candidate in &queued {
        if inflight.threads.contains_key(&candidate.id) {
            continue;
        }
        if start_one(conn, db_path, inflight, &candidate.id)? {
            started += 1;
        }
    }

    Ok(Tick {
        paused: false,
        queued: queued.len(),
        running: task::list_by_status(conn, task::Status::Running)?.len(),
        started,
        inflight: inflight.len(),
        events: drained.consumed,
        resolved: drained.resolved,
    })
}

/// Start one task's next step. Returns whether a step went into flight.
fn start_one(
    conn: &mut Connection,
    db_path: &Path,
    inflight: &mut Inflight,
    task_id: &str,
) -> Result<bool> {
    let task = task::require(conn, task_id)?;

    // Config is per repo root, so it is loaded per task. A task whose policy will
    // not load cannot run, and saying so on the task is more use than a line in a
    // log nobody is reading.
    let policy = match Policy::load(Path::new(&task.repo)) {
        Ok(policy) => policy,
        Err(e) => {
            let reason = format!("policy will not load: {e}");
            tracing::warn!(task = %task_id, "{reason}");
            engine::park_task(conn, task_id, &reason)?;
            return Ok(false);
        }
    };

    match engine::begin_step(conn, &policy, task_id, db_path)? {
        Started::Running(spec) => {
            tracing::info!(
                task = %task_id,
                pipeline = %spec.pipeline,
                step = %spec.step,
                round = spec.round,
                "step started"
            );
            let db_path = db_path.to_path_buf();
            let owned = task_id.to_string();
            let handle = std::thread::spawn(move || run_and_report(db_path, policy, spec, owned));
            inflight.threads.insert(task_id.to_string(), handle);
            Ok(true)
        }
        Started::Finished => {
            tracing::info!(task = %task_id, "task finished");
            Ok(false)
        }
        Started::Parked { reason } => {
            tracing::warn!(task = %task_id, "task parked: {reason}");
            Ok(false)
        }
        Started::Bailed { reason } => {
            tracing::debug!(task = %task_id, "not started: {reason}");
            Ok(false)
        }
    }
}

/// Run a step to completion, then record what it said.
///
/// This is the whole of the supervisor's work: spawn, block, write.
fn run_and_report(db_path: PathBuf, policy: Policy, spec: Box<engine::StepSpec>, task_id: String) {
    let report = engine::run_step(&spec);
    if !report.logs.trim().is_empty() {
        // A step that did not pass is one you will want to read the output of, and
        // the note alone is rarely enough to say why. A pass is noise.
        if report.outcome == crate::Outcome::Pass {
            tracing::debug!(task = %task_id, step = %spec.step, "step output:\n{}", report.logs);
        } else {
            tracing::info!(task = %task_id, step = %spec.step, "step output:\n{}", report.logs);
        }
    }
    tracing::info!(
        task = %task_id,
        step = %spec.step,
        outcome = report.outcome.as_str(),
        note = report.note.as_deref().unwrap_or(""),
        "step finished"
    );

    // A fresh connection: this is another writer, and the store is the only thing
    // these two threads share.
    let result = db::open(&db_path).and_then(|mut conn| {
        engine::finish_step(&mut conn, &policy, &task_id, &spec.at(), &report)
    });
    match result {
        Ok(outcome) => {
            if let crate::engine::TransitionOutcome::Bailed(reason) = outcome {
                tracing::warn!(task = %task_id, "step result discarded: {reason}");
            }
        }
        // Nothing to do but say so: the task stays `running` with no pane, and
        // recovery re-queues it when the supervisor next starts.
        Err(e) => tracing::error!(task = %task_id, "could not record step result: {e}"),
    }
}

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: libc::c_int) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

/// Ask the loop to stop at the end of the current tick. Also what SIGINT and
/// SIGTERM do.
pub fn request_shutdown() {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

pub fn shutdown_requested() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}

fn install_signal_handlers() {
    // Setting an AtomicBool is the only thing the handler does, which is all a
    // signal handler is allowed to do.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as libc::sighandler_t);
    }
}

/// Refuse to start a second supervisor over the same store: two of them would
/// both be advancing the same tasks.
pub fn ensure_sole(conn: &Connection) -> Result<()> {
    match health(conn)? {
        Health::Running { heartbeat, .. } | Health::Stalled { heartbeat, .. } => {
            Err(Error::other(format!(
                "a supervisor is already running as pid {} — stop it first",
                heartbeat.pid
            )))
        }
        Health::Dead { heartbeat, age } => {
            tracing::warn!(
                pid = heartbeat.pid,
                age,
                "clearing a heartbeat left behind by a dead supervisor"
            );
            Ok(())
        }
        Health::Down => Ok(()),
    }
}

/// Poll until asked to stop. `max_ticks` bounds the loop for tests.
pub fn run(conn: &mut Connection, poll: Duration, max_ticks: Option<u64>) -> Result<u64> {
    install_signal_handlers();
    ensure_sole(conn)?;

    let started = db::now();
    beat(conn, started)?;
    let seq = event::append(
        conn,
        &event::NewEvent::new(event::names::SUPERVISOR_STARTED).payload(serde_json::json!({
            "pid": std::process::id(),
            "poll_ms": poll.as_millis(),
        })),
    )?;
    tracing::info!(pid = std::process::id(), seq, "supervisor started");

    // A task left `running` with no bound pane was synchronous and got orphaned
    // when the last supervisor stopped.
    let db_path = PathBuf::from(
        conn.path()
            .ok_or_else(|| Error::other("the store has no path, so steps could not find it"))?,
    );
    let recovered = engine::recover_orphans(conn)?;
    if !recovered.is_empty() {
        tracing::warn!(count = recovered.len(), "re-queued orphaned steps");
    }

    let mut inflight = Inflight::default();
    let mut ticks = 0u64;
    let mut last_beat = started;
    let mut last_report = Tick::default();

    while !shutdown_requested() {
        let observed = tick(conn, &db_path, &mut inflight)?;
        ticks += 1;

        // Only when something changed, so info level stays readable.
        if observed != last_report {
            tracing::info!(
                paused = observed.paused,
                queued = observed.queued,
                running = observed.running,
                inflight = observed.inflight,
                events = observed.events,
                resolved = observed.resolved,
                "tick"
            );
            last_report = observed;
        }

        let now = db::now();
        if now > last_beat {
            beat(conn, started)?;
            last_beat = now;
        }

        if max_ticks.is_some_and(|max| ticks >= max) {
            break;
        }
        std::thread::sleep(poll);
    }

    // Give in-flight steps a moment to report, but do not wait on a `pytest`:
    // abandoning them is safe, because a `running` task with no pane is re-queued
    // on the next start.
    let grace = std::time::Instant::now() + SHUTDOWN_GRACE;
    while !inflight.is_empty() && std::time::Instant::now() < grace {
        inflight.reap();
        std::thread::sleep(Duration::from_millis(20));
    }
    if !inflight.is_empty() {
        tracing::warn!(
            count = inflight.len(),
            "left steps in flight; they will be re-queued on the next start"
        );
    }

    // Clearing the heartbeat is what makes `shep status` instantly correct after
    // a clean stop, rather than correct in five seconds' time.
    meta::delete(conn, meta::HEARTBEAT)?;
    event::append(
        conn,
        &event::NewEvent::new(event::names::SUPERVISOR_STOPPED)
            .payload(serde_json::json!({"pid": std::process::id(), "ticks": ticks})),
    )?;
    tracing::info!(ticks, "supervisor stopped");
    Ok(ticks)
}

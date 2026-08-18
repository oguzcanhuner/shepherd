//! The supervisor: heartbeat, pause flag, and the poll loop.
//!
//! There is no socket and no server (PLAN §7.4). The supervisor's whole job is
//! to poll the store, spawn step scripts and block on them, so aliveness is a
//! row in `meta` rather than a connection you can dial.

use crate::db::{self, event, meta, task};
use crate::{Error, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Default poll interval. Task creation is picked up on the next tick, which is
/// why ~200ms is the number: fast enough that it does not feel deferred.
pub const DEFAULT_POLL: Duration = Duration::from_millis(200);

/// A heartbeat older than this means the supervisor is wedged, not merely busy.
pub const STALE_AFTER: i64 = 5;

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
}

/// One pass over the store.
///
/// M4 is where this starts advancing tasks; for now it observes, so that the
/// loop, the heartbeat and the pause flag can be exercised on their own.
pub fn tick(conn: &mut Connection) -> Result<Tick> {
    if meta::is_paused(conn)? {
        return Ok(Tick {
            paused: true,
            ..Default::default()
        });
    }
    let queued = task::list_by_status(conn, task::Status::Queued)?.len();
    let running = task::list_by_status(conn, task::Status::Running)?.len();
    Ok(Tick {
        paused: false,
        queued,
        running,
    })
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

    // §11: a task left `running` with no bound pane was synchronous and got
    // orphaned when the supervisor died. Re-running it belongs with step
    // spawning, in M4.

    let mut ticks = 0u64;
    let mut last_beat = started;
    let mut last_report = Tick::default();

    while !shutdown_requested() {
        let observed = tick(conn)?;
        ticks += 1;

        // Only when something changed, so info level stays readable.
        if observed != last_report {
            tracing::info!(
                paused = observed.paused,
                queued = observed.queued,
                running = observed.running,
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

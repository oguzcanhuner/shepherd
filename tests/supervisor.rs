//! M1 acceptance: `shep status` is correct with the supervisor up, stopped
//! cleanly, and killed outright.

mod common;

use common::{Store, dead_pid};
use shepherd::db::{meta, task};
use shepherd::supervisor::{self, Health, Heartbeat, Inflight, STALE_AFTER};
use std::time::Duration;

fn write_heartbeat(conn: &rusqlite::Connection, pid: i32, beat: i64) {
    meta::set_json(
        conn,
        meta::HEARTBEAT,
        &Heartbeat {
            pid,
            started: beat - 10,
            beat,
        },
    )
    .expect("write heartbeat");
}

#[test]
fn a_store_nobody_is_supervising_is_down() {
    let store = Store::new();
    assert_eq!(
        supervisor::health(&store.conn()).expect("health"),
        Health::Down
    );
}

#[test]
fn a_fresh_heartbeat_from_a_live_process_is_running() {
    let store = Store::new();
    let conn = store.conn();
    let now = shepherd::db::now();
    write_heartbeat(&conn, std::process::id() as i32, now);

    let health = supervisor::health_at(&conn, now).expect("health");
    assert!(
        matches!(health, Health::Running { age: 0, .. }),
        "got {health:?}"
    );
    assert!(health.is_healthy());
}

#[test]
fn a_heartbeat_from_a_dead_process_is_not_running() {
    let store = Store::new();
    let conn = store.conn();
    let now = shepherd::db::now();
    // A pid that died a moment ago, with a beat too recent to look stale: only
    // the liveness check can tell this apart from a healthy supervisor.
    write_heartbeat(&conn, dead_pid(), now);

    let health = supervisor::health_at(&conn, now).expect("health");
    assert!(matches!(health, Health::Dead { .. }), "got {health:?}");
    assert!(!health.is_healthy());
}

#[test]
fn a_live_process_that_stops_beating_is_stalled() {
    let store = Store::new();
    let conn = store.conn();
    let now = shepherd::db::now();
    write_heartbeat(&conn, std::process::id() as i32, now - STALE_AFTER - 1);

    let health = supervisor::health_at(&conn, now).expect("health");
    assert!(matches!(health, Health::Stalled { .. }), "got {health:?}");
    assert!(!health.is_healthy());
}

#[test]
fn a_beat_from_the_future_is_not_negative_age() {
    let store = Store::new();
    let conn = store.conn();
    let now = shepherd::db::now();
    write_heartbeat(&conn, std::process::id() as i32, now + 60);

    match supervisor::health_at(&conn, now).expect("health") {
        Health::Running { age, .. } => assert_eq!(age, 0),
        other => panic!("expected running, got {other:?}"),
    }
}

#[test]
fn the_supervisor_beats_while_up_and_clears_the_beat_on_the_way_out() {
    let store = Store::new();
    let mut conn = store.conn();

    let ticks = supervisor::run(&mut conn, Duration::from_millis(1), Some(3)).expect("supervise");
    assert_eq!(ticks, 3);

    // Cleared on a clean stop, so status is right immediately rather than in
    // STALE_AFTER seconds' time.
    assert_eq!(supervisor::read_heartbeat(&conn).expect("hb"), None);
    assert_eq!(supervisor::health(&conn).expect("health"), Health::Down);

    let kinds: Vec<String> = shepherd::db::event::recent(&conn, 10)
        .expect("events")
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert!(kinds.contains(&"supervisor.started".to_string()));
    assert!(kinds.contains(&"supervisor.stopped".to_string()));
}

#[test]
fn a_second_supervisor_refuses_to_start() {
    let store = Store::new();
    let conn = store.conn();
    write_heartbeat(&conn, std::process::id() as i32, shepherd::db::now());

    let err = supervisor::ensure_sole(&conn).expect_err("two supervisors would double-advance");
    assert!(err.to_string().contains("already running"), "got {err}");
}

#[test]
fn a_dead_supervisors_heartbeat_does_not_block_a_new_one() {
    let store = Store::new();
    let conn = store.conn();
    write_heartbeat(&conn, dead_pid(), shepherd::db::now());
    supervisor::ensure_sole(&conn).expect("a dead heartbeat is not an obstacle");
}

#[test]
fn a_tick_sees_what_is_queued_and_running() {
    let store = Store::new();
    let repo = common::scripted_repo();
    let root = repo.root().to_string_lossy().to_string();
    store.task_in(&root, "simple", "first");
    let second = store.task_in(&root, "simple", "second");
    let mut conn = store.conn();

    shepherd::engine::transition(&mut conn, &second.id, |_| {
        Ok(shepherd::engine::Decision::apply(
            task::TaskPatch::new().status(task::Status::Running),
        ))
    })
    .expect("claim");

    let tick = supervisor::tick(&mut conn, store.path(), &mut Inflight::default()).expect("tick");
    assert_eq!(tick.queued, 1);
    assert!(!tick.paused);
}

#[test]
fn pausing_stops_the_tick_looking_for_work() {
    let store = Store::new();
    let repo = common::scripted_repo();
    store.task_in(&repo.root().to_string_lossy(), "simple", "do not start me");
    let mut conn = store.conn();

    meta::set_paused(&conn, true).expect("pause");
    let mut inflight = Inflight::default();
    let tick = supervisor::tick(&mut conn, store.path(), &mut inflight).expect("tick");
    assert!(tick.paused);
    assert_eq!(tick.queued, 0, "a paused tick does not go looking for work");
    assert_eq!(tick.started, 0);

    meta::set_paused(&conn, false).expect("resume");
    assert!(!meta::is_paused(&conn).expect("flag"));
    let tick = supervisor::tick(&mut conn, store.path(), &mut inflight).expect("tick");
    assert_eq!(tick.queued, 1);
}

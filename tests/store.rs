//! M1 acceptance: concurrent writers must not lose an update or see
//! SQLITE_BUSY, and state and events must commit as one.

mod common;

use common::Store;
use shepherd::db::task::{Status, TaskPatch};
use shepherd::db::{event, task};
use shepherd::engine::{Decision, TransitionOutcome, transition};
use shepherd::{Error, engine};

/// The M1 hammer: several writers, each on its own connection, incrementing the
/// same counter through `transition()`. Every increment must land — the whole
/// point of re-reading inside `BEGIN IMMEDIATE` — and no writer may fail.
#[test]
fn concurrent_writers_lose_nothing() {
    const WRITERS: i64 = 4;
    const BUMPS: i64 = 150;

    let store = Store::new();
    let task = store.task("hammer the store");

    let mut handles = Vec::new();
    for writer in 0..WRITERS {
        let path = store.path().to_path_buf();
        let task_id = task.id.clone();
        handles.push(std::thread::spawn(move || {
            let mut conn = shepherd::db::open(&path).expect("open");
            for _ in 0..BUMPS {
                let outcome = transition(&mut conn, &task_id, |current| {
                    Ok(Decision::apply(TaskPatch::new().round(current.round + 1)).with_event(
                        event::NewEvent::for_task("task.step_finished", &task_id).payload(
                            serde_json::json!({"step": "bump", "outcome": "pass", "writer": writer}),
                        ),
                    ))
                })
                .expect("no writer should ever fail: WAL + busy_timeout means waiting, not erroring");
                assert!(outcome.is_applied());
            }
        }));
    }
    for h in handles {
        h.join().expect("writer thread panicked");
    }

    let conn = store.conn();
    let final_task = task::require(&conn, &task.id).expect("task");
    assert_eq!(
        final_task.round,
        WRITERS * BUMPS,
        "a lost update means two writers read the same round"
    );

    // One event per applied transition, plus the task.created that made the row.
    let events = event::for_task(&conn, &task.id).expect("events");
    assert_eq!(events.len() as i64, WRITERS * BUMPS + 1);
    assert_eq!(events[0].kind, event::names::TASK_CREATED);

    // seq is the only ordering that matters, and it must be strictly increasing.
    let seqs: Vec<i64> = events.iter().map(|e| e.seq).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "seq must increase");
}

/// Two writers racing to claim the same queued task: exactly one wins, because
/// the loser sees the row already claimed when it re-reads inside the lock.
#[test]
fn only_one_writer_can_claim_a_task() {
    let store = Store::new();
    let task = store.task("claim me once");

    let claim = |conn: &mut rusqlite::Connection| {
        transition(conn, &task.id, |current| {
            if current.status != Status::Queued {
                return Ok(Decision::bail(format!(
                    "already {} — someone else claimed it",
                    current.status
                )));
            }
            Ok(
                Decision::apply(TaskPatch::new().status(Status::Running).step(Some("lint")))
                    .with_event(event::NewEvent::new(event::names::TASK_STEP_STARTED)),
            )
        })
    };

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let path = store.path().to_path_buf();
            let task_id = task.id.clone();
            std::thread::spawn(move || {
                let mut conn = shepherd::db::open(&path).expect("open");
                transition(&mut conn, &task_id, |current| {
                    if current.status != Status::Queued {
                        return Ok(Decision::bail("already claimed"));
                    }
                    Ok(Decision::apply(
                        TaskPatch::new().status(Status::Running).step(Some("lint")),
                    ))
                })
                .expect("claim")
                .is_applied()
            })
        })
        .collect();

    let wins = handles
        .into_iter()
        .map(|h| h.join().expect("thread"))
        .filter(|won| *won)
        .count();
    assert_eq!(wins, 1, "exactly one writer may claim a queued task");

    let mut conn = store.conn();
    assert!(
        !claim(&mut conn).expect("claim").is_applied(),
        "a later claim must bail too"
    );
}

/// A decision that fails writes nothing at all: no state change, no orphan event.
#[test]
fn a_failed_decision_writes_nothing() {
    let store = Store::new();
    let task = store.task("roll me back");
    let mut conn = store.conn();

    let before = task::require(&conn, &task.id).expect("task");
    let events_before = event::for_task(&conn, &task.id).expect("events").len();

    let err = transition(&mut conn, &task.id, |_| {
        Err(Error::other("the step script exploded"))
    })
    .expect_err("the decision failed, so the transition must fail");
    assert!(err.to_string().contains("exploded"));

    let after = task::require(&conn, &task.id).expect("task");
    assert_eq!(before, after, "state must be untouched");
    assert_eq!(
        event::for_task(&conn, &task.id).expect("events").len(),
        events_before,
        "there must never be an event for a change that did not persist"
    );
}

/// Bailing is not an error, and it leaves the row alone.
#[test]
fn bailing_leaves_the_row_alone() {
    let store = Store::new();
    let task = store.task("do not move");
    let mut conn = store.conn();

    let outcome = transition(&mut conn, &task.id, |_| {
        Ok(Decision::bail("the row moved under me"))
    })
    .expect("bail is a normal outcome");

    match outcome {
        TransitionOutcome::Bailed(reason) => assert!(reason.contains("moved")),
        TransitionOutcome::Applied(_) => panic!("expected a bail"),
    }
    assert_eq!(task::require(&conn, &task.id).expect("task"), task);
}

#[test]
fn transitioning_a_missing_task_is_an_error() {
    let store = Store::new();
    let mut conn = store.conn();
    let err = transition(&mut conn, "t-404", |_| {
        Ok(Decision::apply(TaskPatch::new().round(1)))
    })
    .expect_err("no such task");
    assert!(matches!(err, Error::TaskNotFound(id) if id == "t-404"));
}

#[test]
fn ids_are_allocated_in_order_and_do_not_collide() {
    let store = Store::new();
    let ids: Vec<String> = (0..5)
        .map(|i| store.task(&format!("task {i}")).id)
        .collect();
    assert_eq!(ids, vec!["t-1", "t-2", "t-3", "t-4", "t-5"]);
}

#[test]
fn creating_a_task_records_it_as_queued_with_an_event() {
    let store = Store::new();
    let task = store.task("write the thing");
    let conn = store.conn();

    assert_eq!(task.status, Status::Queued);
    assert_eq!(task.round, 0);
    assert!(!task.human_owned);
    assert_eq!(task.pipeline, None);

    let events = event::for_task(&conn, &task.id).expect("events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "task.created");
    assert_eq!(
        events[0].payload.as_ref().and_then(|p| p["brief"].as_str()),
        Some("write the thing")
    );
}

#[test]
fn concurrent_task_creation_gives_every_task_its_own_id() {
    let store = Store::new();
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let path = store.path().to_path_buf();
            std::thread::spawn(move || {
                let mut conn = shepherd::db::open(&path).expect("open");
                engine::create_task(
                    &mut conn,
                    task::NewTask {
                        brief: format!("task {i}"),
                        kind: "feature".into(),
                        repo: "/tmp/repo".into(),
                    },
                )
                .expect("create")
                .id
            })
        })
        .collect();

    let mut ids: Vec<String> = handles
        .into_iter()
        .map(|h| h.join().expect("thread"))
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 8, "id allocation must be inside the write lock");
}

#[test]
fn patches_only_touch_what_they_name() {
    let store = Store::new();
    let task = store.task("partial update");
    let mut conn = store.conn();

    transition(&mut conn, &task.id, |_| {
        Ok(Decision::apply(
            TaskPatch::new()
                .pipeline(Some("review"))
                .step(Some("lint"))
                .round(2),
        ))
    })
    .expect("apply");

    let after = task::require(&conn, &task.id).expect("task");
    assert_eq!(after.pipeline.as_deref(), Some("review"));
    assert_eq!(after.step.as_deref(), Some("lint"));
    assert_eq!(after.round, 2);
    assert_eq!(after.status, Status::Queued, "status was not in the patch");
    assert_eq!(after.brief, task.brief);
    assert!(after.updated >= task.updated);

    // And clearing a column is expressible.
    transition(&mut conn, &task.id, |_| {
        Ok(Decision::apply(TaskPatch::new().step(None::<String>)))
    })
    .expect("clear");
    assert_eq!(task::require(&conn, &task.id).expect("task").step, None);
}

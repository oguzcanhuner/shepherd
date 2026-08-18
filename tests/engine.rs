//! M4 acceptance: a task runs a synchronous pipeline to `finished`, a failing
//! step parks it, and `shep trace` shows why.

mod common;

use common::{Store, scripted_repo};
use shepherd::config::Policy;
use shepherd::db::task::{Status, Task};
use shepherd::db::{event, task};
use shepherd::engine::{self, Started};
use shepherd::supervisor::{self, Inflight};
use std::path::Path;
use std::time::{Duration, Instant};

/// Drive the supervisor loop until a task stops moving, the way the real loop
/// does — start what is queued, let the threads report back.
fn run_until_settled(store: &Store, task_id: &str) -> Task {
    let mut conn = store.conn();
    let mut inflight = Inflight::default();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        supervisor::tick(&mut conn, store.path(), &mut inflight).expect("tick");
        let task = task::require(&conn, task_id).expect("task");
        if inflight.is_empty()
            && matches!(
                task.status,
                Status::Parked | Status::Finished | Status::Cancelled
            )
        {
            return task;
        }
        assert!(
            Instant::now() < deadline,
            "task {task_id} never settled: {task:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn kinds_of(store: &Store, task_id: &str) -> Vec<String> {
    let conn = store.conn();
    event::for_task(&conn, task_id)
        .expect("events")
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

#[test]
fn a_passing_step_runs_the_pipeline_to_finished() {
    let store = Store::new();
    let repo = scripted_repo();
    let task = store.task_in(&repo.root().to_string_lossy(), "simple", "one green step");

    let settled = run_until_settled(&store, &task.id);
    assert_eq!(settled.status, Status::Finished, "got {settled:?}");
    // A finished task is nowhere in particular any more.
    assert_eq!(settled.pipeline, None);
    assert_eq!(settled.step, None);

    assert_eq!(
        kinds_of(&store, &task.id),
        vec![
            "task.created",
            "task.step_started",
            "task.step_finished",
            "task.finished",
        ]
    );
}

#[test]
fn a_step_runs_with_the_environment_the_contract_promises() {
    let store = Store::new();
    let repo = scripted_repo();
    let root = repo.root().to_string_lossy().to_string();
    let task = store.task_in(&root, "simple", "check my environment");

    run_until_settled(&store, &task.id);

    let env = repo.last_env();
    assert_eq!(
        env.get("SHEP_TASK_ID").map(String::as_str),
        Some(task.id.as_str())
    );
    assert_eq!(env.get("SHEP_TYPE").map(String::as_str), Some("simple"));
    assert_eq!(env.get("SHEP_PIPELINE").map(String::as_str), Some("check"));
    assert_eq!(env.get("SHEP_STEP").map(String::as_str), Some("outcome"));
    assert_eq!(env.get("SHEP_ROUND").map(String::as_str), Some("0"));
    assert_eq!(
        env.get("SHEP_REPO").map(String::as_str),
        Some(root.as_str())
    );
    // So that `shep` subcommands a script runs hit the same store.
    assert_eq!(
        env.get("SHEP_DB").map(String::as_str),
        Some(store.path().to_string_lossy().as_ref())
    );
    // Absent, not empty: a script testing -n "$SHEP_WORKTREE" gets the truth.
    assert!(!env.contains_key("SHEP_WORKTREE"), "got {env:?}");
    assert!(!env.contains_key("SHEP_PANE"), "got {env:?}");

    // cwd is the worktree once there is one, and the repo until then.
    assert_eq!(
        std::fs::canonicalize(repo.last_cwd()).expect("cwd"),
        std::fs::canonicalize(&root).expect("root")
    );
}

#[test]
fn a_step_that_errors_parks_the_task_and_the_trace_says_why() {
    let store = Store::new();
    let repo = scripted_repo();
    repo.says(r#"{"outcome":"error","note":"the linter fell over"}"#);
    let task = store.task_in(&repo.root().to_string_lossy(), "simple", "explode please");

    let settled = run_until_settled(&store, &task.id);
    assert_eq!(settled.status, Status::Parked);
    // The position is kept, so `shep retry` knows what to retry.
    assert_eq!(settled.step.as_deref(), Some("outcome"));

    let conn = store.conn();
    let events = event::for_task(&conn, &task.id).expect("events");
    let finished = events
        .iter()
        .find(|e| e.kind == "task.step_finished")
        .expect("a step_finished event");
    assert_eq!(
        finished.payload.as_ref().expect("payload")["outcome"],
        "error"
    );

    let parked = events
        .iter()
        .find(|e| e.kind == "task.parked")
        .expect("a parked event");
    let reason = parked.payload.as_ref().expect("payload")["reason"]
        .as_str()
        .expect("reason");
    assert!(reason.contains("the linter fell over"), "got {reason}");

    // The consequence is linked to the thing that caused it, which is the tree
    // `shep trace` draws.
    assert_eq!(parked.caused_by, Some(finished.seq));
}

#[test]
fn a_non_zero_exit_is_an_error_whatever_the_script_claimed() {
    let store = Store::new();
    let repo = scripted_repo();
    // The lie worth not believing: a failing script that still says pass.
    repo.says(r#"{"outcome":"pass"}"#).exits(3);
    let task = store.task_in(&repo.root().to_string_lossy(), "simple", "lie to me");

    let settled = run_until_settled(&store, &task.id);
    assert_eq!(settled.status, Status::Parked);

    let conn = store.conn();
    let note = event::for_task(&conn, &task.id)
        .expect("events")
        .into_iter()
        .find(|e| e.kind == "task.step_finished")
        .and_then(|e| e.payload)
        .map(|p| p["note"].as_str().unwrap_or_default().to_string())
        .expect("a note");
    assert!(note.contains("status 3"), "got {note}");
}

#[test]
fn output_that_is_not_a_verdict_is_an_error() {
    let store = Store::new();
    let repo = scripted_repo();
    repo.says("all done!");
    let task = store.task_in(
        &repo.root().to_string_lossy(),
        "simple",
        "say something else",
    );

    assert_eq!(run_until_settled(&store, &task.id).status, Status::Parked);

    let conn = store.conn();
    let note = event::for_task(&conn, &task.id)
        .expect("events")
        .into_iter()
        .find(|e| e.kind == "task.step_finished")
        .and_then(|e| e.payload)
        .map(|p| p["note"].as_str().unwrap_or_default().to_string())
        .expect("a note");
    assert!(note.contains("not a verdict"), "got {note}");
    assert!(
        note.contains("all done!"),
        "the note quotes what was printed"
    );
}

#[test]
fn an_unknown_outcome_word_is_an_error() {
    let store = Store::new();
    let repo = scripted_repo();
    repo.says(r#"{"outcome":"maybe"}"#);
    let task = store.task_in(
        &repo.root().to_string_lossy(),
        "simple",
        "invent an outcome",
    );
    assert_eq!(run_until_settled(&store, &task.id).status, Status::Parked);
}

#[test]
fn a_rejection_with_nowhere_to_go_parks() {
    let store = Store::new();
    let repo = scripted_repo();
    repo.says(r#"{"outcome":"reject","note":"two lint errors"}"#);
    let task = store.task_in(&repo.root().to_string_lossy(), "simple", "reject me");

    let settled = run_until_settled(&store, &task.id);
    assert_eq!(settled.status, Status::Parked);

    let conn = store.conn();
    let reason = event::for_task(&conn, &task.id)
        .expect("events")
        .into_iter()
        .find(|e| e.kind == "task.parked")
        .and_then(|e| e.payload)
        .map(|p| p["reason"].as_str().unwrap_or_default().to_string())
        .expect("a reason");
    assert!(reason.contains("no on_fail"), "got {reason}");
}

#[test]
fn a_promise_nothing_can_resolve_parks() {
    let store = Store::new();
    let repo = scripted_repo();
    // `started` is a promise, and this pipeline has no await to keep it.
    repo.says(r#"{"outcome":"started","pane":"wA:p2"}"#);
    let task = store.task_in(&repo.root().to_string_lossy(), "simple", "promise me");

    let settled = run_until_settled(&store, &task.id);
    assert_eq!(settled.status, Status::Parked);

    let conn = store.conn();
    let reason = event::for_task(&conn, &task.id)
        .expect("events")
        .into_iter()
        .find(|e| e.kind == "task.parked")
        .and_then(|e| e.payload)
        .map(|p| p["reason"].as_str().unwrap_or_default().to_string())
        .expect("a reason");
    assert!(reason.contains("has no await"), "got {reason}");
}

#[test]
fn retry_runs_the_step_again() {
    let store = Store::new();
    let repo = scripted_repo();
    repo.says(r#"{"outcome":"error","note":"not today"}"#);
    let task = store.task_in(&repo.root().to_string_lossy(), "simple", "fail then pass");
    assert_eq!(run_until_settled(&store, &task.id).status, Status::Parked);

    // Fix whatever it was, then retry: the task picks up at the step it stopped on.
    repo.says(r#"{"outcome":"pass"}"#);
    let mut conn = store.conn();
    assert!(
        engine::retry(&mut conn, &task.id)
            .expect("retry")
            .is_applied()
    );

    assert_eq!(run_until_settled(&store, &task.id).status, Status::Finished);
    let kinds = kinds_of(&store, &task.id);
    assert_eq!(
        kinds.iter().filter(|k| *k == "task.step_finished").count(),
        2
    );
    assert!(kinds.contains(&"task.resumed".to_string()));
}

#[test]
fn only_a_parked_task_can_be_retried() {
    let store = Store::new();
    let task = store.task("still queued");
    let mut conn = store.conn();

    match engine::retry(&mut conn, &task.id).expect("retry") {
        shepherd::engine::TransitionOutcome::Bailed(reason) => {
            assert!(reason.contains("only a parked task"), "got {reason}")
        }
        other => panic!("expected a bail, got {other:?}"),
    }
}

#[test]
fn cancelling_stops_a_task_for_good() {
    let store = Store::new();
    let task = store.task("never mind");
    let mut conn = store.conn();

    assert!(
        engine::cancel(&mut conn, &task.id, Some("changed my mind".into()))
            .expect("cancel")
            .is_applied()
    );
    assert_eq!(
        task::require(&conn, &task.id).expect("task").status,
        Status::Cancelled
    );

    // And cancelling twice is a no-op rather than an error to handle.
    match engine::cancel(&mut conn, &task.id, None).expect("cancel") {
        shepherd::engine::TransitionOutcome::Bailed(reason) => {
            assert!(reason.contains("already cancelled"), "got {reason}")
        }
        other => panic!("expected a bail, got {other:?}"),
    }
}

#[test]
fn a_task_whose_policy_will_not_load_parks_with_the_reason() {
    let store = Store::new();
    let repo = scripted_repo();
    // Break the config after the task was created — the case where a task is
    // already queued when someone edits policy.
    repo.write("[pipeline.check]\nsteps = [\"nope\"]\n");
    let task = store.task_in(&repo.root().to_string_lossy(), "simple", "broken policy");

    let settled = run_until_settled(&store, &task.id);
    assert_eq!(settled.status, Status::Parked);

    let conn = store.conn();
    let reason = event::for_task(&conn, &task.id)
        .expect("events")
        .into_iter()
        .find(|e| e.kind == "task.parked")
        .and_then(|e| e.payload)
        .map(|p| p["reason"].as_str().unwrap_or_default().to_string())
        .expect("a reason");
    assert!(reason.contains("policy will not load"), "got {reason}");
    assert!(reason.contains("nope"), "the reason names the broken step");
}

#[test]
fn an_orphaned_synchronous_step_is_requeued() {
    let store = Store::new();
    let repo = scripted_repo();
    let root = repo.root().to_string_lossy().to_string();

    // A task stuck in `running`, as a supervisor that died mid-step would leave
    // it. Its pipeline has no `await`, so nothing but the supervisor was ever
    // going to report on it (PLAN §11).
    let orphan = store.task_in(&root, "simple", "orphaned");
    let mut conn = store.conn();
    engine::transition(&mut conn, &orphan.id, |_| {
        Ok(shepherd::engine::Decision::apply(
            shepherd::db::task::TaskPatch::new()
                .status(Status::Running)
                .pipeline(Some("check"))
                .step(Some("outcome")),
        ))
    })
    .expect("claim");

    let recovered = engine::recover_orphans(&mut conn).expect("recover");
    assert_eq!(recovered, vec![orphan.id.clone()]);
    assert_eq!(
        task::require(&conn, &orphan.id).expect("task").status,
        Status::Queued
    );

    // A deferred step, by contrast, is still out there — see tests/deferred.rs.
}

#[test]
fn a_stale_step_report_is_discarded() {
    let store = Store::new();
    let repo = scripted_repo();
    let root = repo.root().to_string_lossy().to_string();
    let task = store.task_in(&root, "simple", "who moved my step");
    let policy = Policy::load(Path::new(&root)).expect("policy");
    let mut conn = store.conn();

    let Started::Running(spec) =
        engine::begin_step(&mut conn, &policy, &task.id, store.path()).expect("begin")
    else {
        panic!("expected a step to start");
    };

    // Someone cancelled the task while the step was in flight.
    engine::cancel(&mut conn, &task.id, None).expect("cancel");

    let report = shepherd::engine::run_step(&spec);
    match engine::finish_step(&mut conn, &policy, &task.id, &spec.at(), &report).expect("finish") {
        shepherd::engine::TransitionOutcome::Bailed(reason) => {
            assert!(reason.contains("moved on"), "got {reason}")
        }
        other => panic!("a report about a task that moved must be discarded, got {other:?}"),
    }
    assert_eq!(
        task::require(&conn, &task.id).expect("task").status,
        Status::Cancelled
    );
}

// ------------------------------------------------------- composition of steps

/// A repo whose steps record the order they ran in.
fn ordered_repo(config: &str) -> common::Repo {
    let repo = common::Repo::new();
    for step in ["one", "two", "three", "wrap_up"] {
        repo.recording_script(step);
    }
    repo.write(config);
    repo
}

#[test]
fn the_steps_of_a_pipeline_run_in_order() {
    let store = Store::new();
    let repo = ordered_repo(
        r#"
[pipeline.check]
steps = ["one", "two", "three"]

[type.simple]
description = "three steps, in order"
pipelines = ["check"]
"#,
    );
    let task = store.task_in(&repo.root().to_string_lossy(), "simple", "in order please");

    assert_eq!(run_until_settled(&store, &task.id).status, Status::Finished);
    assert_eq!(repo.order(), vec!["one", "two", "three"]);
    assert_eq!(
        repo.positions(),
        vec![
            "check/one round 0",
            "check/two round 0",
            "check/three round 0"
        ]
    );
}

#[test]
fn a_type_runs_each_of_its_pipelines() {
    let store = Store::new();
    let repo = ordered_repo(
        r#"
[pipeline.first]
steps = ["one", "two"]

[pipeline.second]
steps = ["three"]

[type.simple]
description = "two pipelines, run in sequence"
pipelines = ["first", "second"]
"#,
    );
    let task = store.task_in(&repo.root().to_string_lossy(), "simple", "both pipelines");

    assert_eq!(run_until_settled(&store, &task.id).status, Status::Finished);
    assert_eq!(repo.order(), vec!["one", "two", "three"]);
    assert_eq!(
        repo.positions(),
        vec![
            "first/one round 0",
            "first/two round 0",
            "second/three round 0"
        ]
    );
}

#[test]
fn a_nested_pipeline_runs_and_hands_back_to_its_parent() {
    let store = Store::new();
    let repo = ordered_repo(
        r#"
[pipeline.inner]
steps = ["two", "three"]

[pipeline.outer]
steps = ["one", "inner", "wrap_up"]

[type.simple]
description = "a pipeline used as a step, for its outcome"
pipelines = ["outer"]
"#,
    );
    let task = store.task_in(&repo.root().to_string_lossy(), "simple", "nest me");

    assert_eq!(run_until_settled(&store, &task.id).status, Status::Finished);
    assert_eq!(repo.order(), vec!["one", "two", "three", "wrap_up"]);
    // Round is scoped to the innermost pipeline, and the nested steps say so.
    assert_eq!(
        repo.positions(),
        vec![
            "outer/one round 0",
            "inner/two round 0",
            "inner/three round 0",
            "outer/wrap_up round 0"
        ]
    );
}

//! Rest between pipelines: a task runs its plan, comes to rest, and a person or
//! the orchestrator applies the next pipeline by hand. Humans are not in the
//! state machine — there is no `await = "human"`, no approve/reject — so a
//! handoff is simply the task resting with a live pane you can talk to.

mod common;

use common::{Repo, Store};
use shepherd::db::task::{Status, Task};
use shepherd::db::task;
use shepherd::supervisor::{self, Inflight};
use std::time::{Duration, Instant};

const SHEP: &str = env!("CARGO_BIN_EXE_shep");

/// A repo whose type is a single review pipeline. `integrate` exists but is not
/// in the type — it is applied by hand after the task rests.
fn handed_repo() -> Repo {
    let repo = Repo::new();
    repo.recording_script("check_it");
    repo.recording_script("land");
    repo.write(
        r#"
[pipeline.review]
steps = ["check_it"]

[pipeline.integrate]
steps = ["land"]

[type.feature]
description = "Reviewed, then it rests for you."
pipelines = ["review"]
"#,
    );
    repo.git_init();
    repo
}

fn drive(store: &Store, inflight: &mut Inflight, task_id: &str) -> Task {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last = None;
    loop {
        let mut conn = store.conn();
        supervisor::tick(&mut conn, store.path(), inflight).expect("tick");
        let task = task::require(&conn, task_id).expect("task");
        let here = (task.status, task.pipeline.clone(), task.step.clone());
        if inflight.is_empty() && last.as_ref() == Some(&here) {
            return task;
        }
        last = Some(here);
        assert!(Instant::now() < deadline, "never settled: {task:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Run a task until its plan is spent and it rests.
fn rested(store: &Store, repo: &Repo) -> (Task, Inflight) {
    let root = repo.root().to_string_lossy().to_string();
    let created = store.task_in(&root, "feature", "something for you to look at");
    let mut inflight = Inflight::default();
    let task = drive(store, &mut inflight, &created.id);
    assert_eq!(task.status, Status::Resting, "plan spent, so it rests");
    (task, inflight)
}

fn shep(store: &Store, args: &[&str]) -> std::process::Output {
    std::process::Command::new(SHEP)
        .arg("--db")
        .arg(store.path())
        .args(args)
        .env_remove("SHEP_TASK_ID")
        .env_remove("HERDR_PANE_ID")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run shep")
}

fn ok(store: &Store, args: &[&str]) -> String {
    let out = shep(store, args);
    assert!(
        out.status.success(),
        "shep {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn a_task_rests_when_its_plan_is_spent() {
    let store = Store::new();
    let repo = handed_repo();
    let (task, _inflight) = rested(&store, &repo);

    // Resting is not terminal, and it is nowhere in particular.
    assert!(!task.status.is_terminal());
    assert_eq!(task.pipeline, None);
    assert_eq!(task.step, None);
    assert_eq!(repo.order(), vec!["check_it"], "only the review ran");
}

#[test]
fn run_applies_a_pipeline_to_a_resting_task_and_it_rests_again() {
    let store = Store::new();
    let repo = handed_repo();
    let (task, mut inflight) = rested(&store, &repo);

    // `integrate` is not in the feature type, but any defined pipeline can be
    // applied by hand — "what's next" lives on the task now, not the type.
    let out = ok(&store, &["run", "integrate", "--task", &task.id]);
    assert!(out.contains("integrate/land"), "got {out}");

    let done = drive(&store, &mut inflight, &task.id);
    assert_eq!(done.status, Status::Resting, "and it rests again after");
    assert_eq!(
        repo.order(),
        vec!["check_it", "land"],
        "the applied pipeline ran on top of what came before"
    );
}

#[test]
fn an_undefined_pipeline_is_refused() {
    let store = Store::new();
    let repo = handed_repo();
    let (task, _inflight) = rested(&store, &repo);

    let out = shep(&store, &["run", "nonsense", "--task", &task.id]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nonsense"),
        "got {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

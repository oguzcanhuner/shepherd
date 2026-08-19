//! M6 acceptance: a pipeline that loops. A rejection goes to the repair step, the
//! round goes up, and a cap is what stops it — a deliberately bad implementation
//! loops twice and then exhausts.
//!
//! No agents in here. What `agent_review` and `fix` are is a script's business
//!; what a rejection *means* is the engine's, and that is what these
//! test.

mod common;

use common::{Repo, Store};
use shepherd::db::check::{Conclusion, NewCheck};
use shepherd::db::task::{Status, Task};
use shepherd::db::{check, event, pane, raw_event, task};
use shepherd::supervisor::{self, Inflight};
use std::time::{Duration, Instant};

const SHEP: &str = env!("CARGO_BIN_EXE_shep");

/// A step that records where it ran and says whatever the test told it to.
///
/// `.shep/verdict-<step>.<round>` wins over `.shep/verdict-<step>`, which is how
/// "bad in round 0, fixed by round 1" is staged without a second script.
fn verdict_step(repo: &Repo, name: &str) {
    repo.script_with(
        name,
        r#"echo "$SHEP_PIPELINE/$SHEP_STEP round $SHEP_ROUND" >> "$SHEP_REPO/.shep/positions"
verdict="$SHEP_REPO/.shep/verdict-$SHEP_STEP"
if [ -f "$verdict.$SHEP_ROUND" ]; then
  cat "$verdict.$SHEP_ROUND"
elif [ -f "$verdict" ]; then
  cat "$verdict"
else
  echo '{"outcome":"pass"}'
fi"#,
    );
}

/// What a step will say, for every round or for one of them.
fn says(repo: &Repo, step: &str, verdict: &str) {
    std::fs::write(
        repo.root().join(format!(".shep/verdict-{step}")),
        format!("{verdict}\n"),
    )
    .expect("write verdict");
}

fn says_in_round(repo: &Repo, step: &str, round: i64, verdict: &str) {
    std::fs::write(
        repo.root().join(format!(".shep/verdict-{step}.{round}")),
        format!("{verdict}\n"),
    )
    .expect("write verdict");
}

const REJECT: &str = r#"{"outcome":"reject","note":"not good enough"}"#;

/// A repo whose `review` loops, followed by a pipeline that proves the type
/// carried on.
fn looping_repo(review: &str) -> Repo {
    let repo = Repo::new();
    for step in ["lint", "test", "fix", "ship"] {
        verdict_step(&repo, step);
    }
    repo.write(&format!(
        r#"
{review}

[pipeline.after]
steps = ["ship"]

[type.reviewed]
description = "Reviewed until it passes, or until the rounds run out."
pipelines = ["review", "after"]
"#
    ));
    repo
}

fn run(store: &Store, repo: &Repo, brief: &str) -> Task {
    let root = repo.root().to_string_lossy().to_string();
    let created = store.task_in(&root, "reviewed", brief);
    let mut inflight = Inflight::default();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        {
            let mut conn = store.conn();
            supervisor::tick(&mut conn, store.path(), &mut inflight).expect("tick");
            let task = task::require(&conn, &created.id).expect("task");
            if inflight.is_empty() && matches!(task.status, Status::Parked | Status::Resting) {
                return task;
            }
        }
        assert!(Instant::now() < deadline, "task never settled");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn park_reason(store: &Store, task_id: &str) -> String {
    let conn = store.conn();
    event::for_task(&conn, task_id)
        .expect("events")
        .into_iter()
        .rev()
        .find(|e| e.kind == "task.parked")
        .and_then(|e| e.payload)
        .map(|p| p["reason"].as_str().unwrap_or_default().to_string())
        .expect("a parked event with a reason")
}

// --------------------------------------------------------------- the loop

const THREE_ROUNDS: &str = r#"
[pipeline.review]
steps        = ["lint", "test"]
on_fail      = "fix"
max_rounds   = 3
on_exhausted = "reject"
"#;

#[test]
fn a_rejection_goes_to_the_repair_step_and_round_again() {
    let store = Store::new();
    let repo = looping_repo(THREE_ROUNDS);
    // Bad once. Whatever `fix` did, the second round is clean.
    says_in_round(&repo, "test", 0, REJECT);

    let task = run(&store, &repo, "bad on the first pass");

    assert_eq!(task.status, Status::Resting);
    assert_eq!(
        repo.positions(),
        vec![
            "review/lint round 0",
            "review/test round 0",
            // The repair step is reached at the round the rejection moved to...
            "review/fix round 1",
            // ...and then the pipeline is tried again from the top, in that round.
            "review/lint round 1",
            "review/test round 1",
            // A new pipeline starts its own count: round is scoped to the
            // innermost pipeline.
            "after/ship round 0",
        ]
    );
}

#[test]
fn a_bad_implementation_loops_twice_and_then_exhausts() {
    let store = Store::new();
    let repo = looping_repo(THREE_ROUNDS);
    // Never fixed, however many chances it gets.
    says(&repo, "test", REJECT);

    let task = run(&store, &repo, "wrong, and staying wrong");

    assert_eq!(task.status, Status::Parked);
    assert_eq!(
        repo.positions(),
        vec![
            "review/lint round 0",
            "review/test round 0",
            "review/fix round 1",
            "review/lint round 1",
            "review/test round 1",
            "review/fix round 2",
            "review/lint round 2",
            "review/test round 2",
        ],
        "three rounds of the pipeline, so two goes at repairing it"
    );
    let reason = park_reason(&store, &task.id);
    assert!(reason.contains("all 3 rounds"), "got {reason}");
    assert!(reason.contains("rejected this task"), "got {reason}");
    assert_eq!(task.round, 2, "and the row says how far it got");
}

#[test]
fn one_round_means_one_go_and_no_repair() {
    let store = Store::new();
    let repo = looping_repo(
        r#"
[pipeline.review]
steps        = ["lint"]
on_fail      = "fix"
max_rounds   = 1
on_exhausted = "reject"
"#,
    );
    says(&repo, "lint", REJECT);

    let task = run(&store, &repo, "one chance");

    assert_eq!(task.status, Status::Parked);
    assert_eq!(
        repo.positions(),
        vec!["review/lint round 0"],
        "a cap of one is a pipeline that does not repair"
    );
}

#[test]
fn an_exhausted_pipeline_can_be_told_to_pass() {
    let store = Store::new();
    let repo = looping_repo(
        r#"
[pipeline.review]
steps        = ["lint"]
on_fail      = "fix"
max_rounds   = 2
on_exhausted = "pass"
"#,
    );
    says(&repo, "lint", REJECT);

    let task = run(&store, &repo, "advisory review");

    assert_eq!(
        task.status,
        Status::Resting,
        "on_exhausted is the pipeline's own outcome, so it can be a pass"
    );
    assert_eq!(
        repo.positions(),
        vec![
            "review/lint round 0",
            "review/fix round 1",
            "review/lint round 1",
            "after/ship round 0",
        ]
    );
}

#[test]
fn without_on_exhausted_a_spent_pipeline_rejects() {
    let store = Store::new();
    let repo = looping_repo(
        r#"
[pipeline.review]
steps      = ["lint"]
on_fail    = "fix"
max_rounds = 2
"#,
    );
    says(&repo, "lint", REJECT);

    let task = run(&store, &repo, "no exhaustion clause");

    assert_eq!(
        task.status,
        Status::Parked,
        "calling it a pass would wave through the very thing it was checking"
    );
    let reason = park_reason(&store, &task.id);
    assert!(reason.contains("all 2 rounds"), "got {reason}");
}

#[test]
fn a_repair_step_that_breaks_parks_the_task() {
    let store = Store::new();
    let repo = looping_repo(THREE_ROUNDS);
    says(&repo, "test", REJECT);
    says(&repo, "fix", r#"{"outcome":"error","note":"no idea how"}"#);

    let task = run(&store, &repo, "unrepairable");

    assert_eq!(task.status, Status::Parked);
    let reason = park_reason(&store, &task.id);
    assert!(reason.contains("no idea how"), "got {reason}");
    assert_eq!(
        repo.positions(),
        vec![
            "review/lint round 0",
            "review/test round 0",
            "review/fix round 1"
        ],
        "an error stops the loop rather than spending its rounds"
    );
}

#[test]
fn a_repair_step_that_rejects_spends_the_rounds_like_any_other() {
    let store = Store::new();
    let repo = looping_repo(THREE_ROUNDS);
    says(&repo, "lint", REJECT);
    says(&repo, "fix", REJECT);

    let task = run(&store, &repo, "repairs that report failure");

    // on_fail is where *any* rejection in this pipeline goes, including the repair
    // step's own. The cap is what makes that terminate.
    assert_eq!(task.status, Status::Parked);
    assert_eq!(
        repo.positions(),
        vec![
            "review/lint round 0",
            "review/fix round 1",
            "review/fix round 2",
        ]
    );
}

#[test]
fn a_rejection_with_nowhere_to_go_still_parks() {
    let store = Store::new();
    let repo = looping_repo(
        r#"
[pipeline.review]
steps = ["lint"]
"#,
    );
    says(&repo, "lint", REJECT);

    let task = run(&store, &repo, "no loop at all");

    assert_eq!(task.status, Status::Parked);
    let reason = park_reason(&store, &task.id);
    assert!(reason.contains("has no on_fail"), "got {reason}");
}

// ------------------------------------------ what the rounds do to the record

#[test]
fn each_round_is_judged_on_its_own_checks() {
    let store = Store::new();
    let repo = looping_repo(THREE_ROUNDS);
    says_in_round(&repo, "test", 0, REJECT);
    let task = run(&store, &repo, "checks from two rounds");

    // Two rounds of `test` ran, and a check from each: what resolves a step is the
    // latest for *that round*, so a verdict cannot leak forwards.
    let conn = store.conn();
    for (round, conclusion) in [(0, Conclusion::Fail), (1, Conclusion::Pass)] {
        check::insert(
            &conn,
            &NewCheck {
                task_id: task.id.clone(),
                pipeline: Some("review".to_string()),
                step: Some("test".to_string()),
                round: Some(round),
                author: "pytest".to_string(),
                sha: "beefbeef".to_string(),
                conclusion,
                body: None,
            },
        )
        .expect("insert");
    }
    assert_eq!(
        check::latest_for_step(&conn, &task.id, "review", "test", 0)
            .expect("check")
            .map(|c| c.conclusion),
        Some(Conclusion::Fail)
    );
    assert_eq!(
        check::latest_for_step(&conn, &task.id, "review", "test", 1)
            .expect("check")
            .map(|c| c.conclusion),
        Some(Conclusion::Pass)
    );
}

#[test]
fn a_deferred_step_that_fails_goes_round_again() {
    let store = Store::new();
    let repo = Repo::new();
    // `launch` stands in for `code`: it binds a pane and promises. Its verdict
    // arrives as a check, so this is where M5 and M6 meet — the round the check was
    // written for is the round it can resolve.
    let launch = format!(
        r#"echo "$SHEP_PIPELINE/$SHEP_STEP round $SHEP_ROUND" >> "$SHEP_REPO/.shep/positions"
{SHEP} bind-pane "wZ:p1" --workspace wZ >/dev/null || exit 1
printf '{{"outcome":"started","pane":"wZ:p1"}}\n'"#
    );
    repo.script_with("launch", &launch);
    verdict_step(&repo, "redo");
    repo.write(
        r#"
[pipeline.implement]
steps        = [{ run = "launch", await = "agent_stopped" }]
on_fail      = "redo"
max_rounds   = 2
on_exhausted = "reject"

[type.watched]
description = "An agent, with one chance to be told to try again."
pipelines = ["implement"]
"#,
    );

    let root = repo.root().to_string_lossy().to_string();
    let task = store.task_in(&root, "watched", "an agent that gets it wrong");
    let mut inflight = Inflight::default();
    let mut conn = store.conn();
    supervisor::tick(&mut conn, store.path(), &mut inflight).expect("tick");
    // Let the step report its promise.
    let deadline = Instant::now() + Duration::from_secs(10);
    while task::require(&conn, &task.id).expect("task").step.is_none()
        || !inflight.is_empty()
        || pane::for_task(&conn, &task.id).expect("pane").is_none()
    {
        supervisor::tick(&mut conn, store.path(), &mut inflight).expect("tick");
        assert!(Instant::now() < deadline, "the step never got going");
        std::thread::sleep(Duration::from_millis(10));
    }

    // Round 0: the agent says it could not do it.
    check::insert(
        &conn,
        &NewCheck {
            task_id: task.id.clone(),
            pipeline: Some("implement".to_string()),
            step: Some("launch".to_string()),
            round: Some(0),
            author: "claude".to_string(),
            sha: "cafecafe".to_string(),
            conclusion: Conclusion::Fail,
            body: Some("I could not work out what was wanted".to_string()),
        },
    )
    .expect("insert");
    raw_event::append(
        &conn,
        r#"{"event":"pane_agent_status_changed","data":{"pane_id":"wZ:p1","agent_status":"working"}}"#,
    )
    .expect("append");
    raw_event::append(
        &conn,
        r#"{"event":"pane_agent_status_changed","data":{"pane_id":"wZ:p1","agent_status":"done"}}"#,
    )
    .expect("append");
    drop(conn);

    // A failed check is a rejection, so the round goes up and `redo` runs. When
    // `redo` passes, the pipeline is tried again from the top — and `launch` is
    // waiting on a verdict once more, this time for round 1.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let mut conn = store.conn();
        supervisor::tick(&mut conn, store.path(), &mut inflight).expect("tick");
        let now = task::require(&conn, &task.id).expect("task");
        let positions = repo.positions();
        if inflight.is_empty() && positions.len() >= 3 {
            assert_eq!(
                positions,
                vec![
                    "implement/launch round 0",
                    "implement/redo round 1",
                    "implement/launch round 1",
                ]
            );
            assert_eq!(now.status, Status::Running);
            assert_eq!(now.round, 1);
            // The round-0 check judged round-0 work. Leaving it able to resolve
            // round 1 would mean the second attempt inherited the first's verdict.
            assert!(
                check::latest_for_step(&conn, &task.id, "implement", "launch", 1)
                    .expect("check")
                    .is_none()
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "never got round again: {now:?} {positions:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

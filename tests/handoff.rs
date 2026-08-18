//! M7 acceptance: you can talk to the agent through a whole handoff without the
//! state machine moving, re-run review by hand, and then approve.

mod common;

use common::{Repo, Store};
use shepherd::db::task::{Status, Task};
use shepherd::db::{check, event, pane, raw_event, task};
use shepherd::supervisor::{self, Inflight};
use std::path::Path;
use std::time::{Duration, Instant};

const SHEP: &str = env!("CARGO_BIN_EXE_shep");

/// A repo whose type is review, then a handoff, then a last step that proves the
/// approval carried it on.
fn handed_repo() -> Repo {
    let repo = Repo::new();
    repo.recording_script("check_it");
    repo.recording_script("land");
    // The handoff step: binds a pane the way `show_diff.sh` does, then promises.
    repo.script_with(
        "show_diff",
        &format!(
            r#"echo "$SHEP_PIPELINE/$SHEP_STEP round $SHEP_ROUND" >> "$SHEP_REPO/.shep/positions"
{SHEP} bind-pane "wH:p1" --workspace wH >/dev/null || exit 1
printf '{{"outcome":"started","pane":"wH:p1"}}\n'"#
        ),
    );
    repo.write(
        r#"
[pipeline.review]
steps = ["check_it"]

[pipeline.handoff]
steps = ["show_diff"]
await = "human"

[pipeline.integrate]
steps = ["land"]

[type.feature]
description = "Reviewed, shown to you, then landed."
pipelines = ["review", "handoff", "integrate"]
"#,
    );
    // Committed last: a check is a verdict about a commit, so there has to be one.
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

/// Run a task up to the handoff, where it should sit indefinitely.
fn handed_over(store: &Store, repo: &Repo) -> (Task, Inflight) {
    let root = repo.root().to_string_lossy().to_string();
    let created = store.task_in(&root, "feature", "something for you to look at");
    let mut inflight = Inflight::default();
    let task = drive(store, &mut inflight, &created.id);

    assert_eq!(task.status, Status::Running);
    assert_eq!(task.step.as_deref(), Some("show_diff"));
    assert!(
        task.human_owned,
        "a handoff is yours, and being yours is what mutes it"
    );
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

fn agent_said(store: &Store, pane: &str, status: &str) {
    let conn = store.conn();
    raw_event::append(
        &conn,
        &format!(
            r#"{{"event":"pane_agent_status_changed","data":{{"pane_id":"{pane}","agent_status":"{status}"}}}}"#
        ),
    )
    .expect("append");
}

#[test]
fn a_whole_conversation_with_the_agent_moves_nothing() {
    let store = Store::new();
    let repo = handed_repo();
    let (task, mut inflight) = handed_over(&store, &repo);

    // Talking to the agent: it goes busy and quiet, over and over, and a check
    // even lands for the step it is sitting on. None of that is an answer.
    for _ in 0..3 {
        agent_said(&store, "wH:p1", "working");
        agent_said(&store, "wH:p1", "done");
    }
    ok(&store, &["check", "submit", "--pass", "--task", &task.id]);

    let after = drive(&store, &mut inflight, &task.id);
    assert_eq!(after.step.as_deref(), Some("show_diff"));
    assert!(after.human_owned);
    assert_eq!(
        repo.order(),
        vec!["check_it"],
        "only the review that ran before the handoff"
    );

    // The events were kept, they just did not decide anything.
    let conn = store.conn();
    assert_eq!(
        pane::last_status(&conn, "wH:p1")
            .expect("status")
            .as_deref(),
        Some("done"),
        "muted is not unread"
    );
}

#[test]
fn approving_carries_the_task_on_and_leaves_a_verdict() {
    let store = Store::new();
    let repo = handed_repo();
    let (task, mut inflight) = handed_over(&store, &repo);

    let out = ok(
        &store,
        &[
            "approve",
            "--task",
            &task.id,
            "--author",
            "oguz",
            "--note",
            "read it; happy",
        ],
    );
    assert!(out.contains("pass"), "got {out}");

    let done = drive(&store, &mut inflight, &task.id);
    assert_eq!(done.status, Status::Finished);
    assert!(
        !done.human_owned,
        "the task is the machine's again the moment it is not waiting for you"
    );
    assert_eq!(
        repo.order(),
        vec!["check_it", "land"],
        "and the type carried on past the handoff"
    );

    // A person approving is a verdict about a commit like any other, and it is
    // stamped with the commit it was about.
    let conn = store.conn();
    let human = check::for_task(&conn, &task.id)
        .expect("checks")
        .into_iter()
        .find(|c| c.author == "oguz")
        .expect("a check from the person who approved");
    assert_eq!(human.conclusion, check::Conclusion::Pass);
    assert_eq!(human.sha, repo.head());
    assert_eq!(human.step.as_deref(), Some("show_diff"));
    assert_eq!(human.body.as_deref(), Some("read it; happy"));
}

#[test]
fn rejecting_sends_it_where_the_pipeline_sends_a_rejection() {
    let store = Store::new();
    let repo = handed_repo();
    let (task, mut inflight) = handed_over(&store, &repo);

    ok(
        &store,
        &["reject", "--task", &task.id, "--note", "not this"],
    );
    let after = drive(&store, &mut inflight, &task.id);

    // `handoff` has no on_fail, so a rejection parks it — inert, and waiting for
    // whatever you decide to do next.
    assert_eq!(after.status, Status::Parked);
    assert!(!after.human_owned);
    let reason = event::for_task(&store.conn(), &task.id)
        .expect("events")
        .into_iter()
        .rev()
        .find(|e| e.kind == "task.parked")
        .and_then(|e| e.payload)
        .map(|p| p["reason"].as_str().unwrap_or_default().to_string())
        .expect("a reason");
    assert!(reason.contains("has no on_fail"), "got {reason}");
}

#[test]
fn review_can_be_run_again_by_hand_and_comes_back_to_you() {
    let store = Store::new();
    let repo = handed_repo();
    let (task, mut inflight) = handed_over(&store, &repo);

    // The middle of the acceptance: you read the diff and want the checks run
    // again before you say anything.
    let out = ok(&store, &["run", "review", "--task", &task.id]);
    assert!(out.contains("review/check_it"), "got {out}");

    let back = drive(&store, &mut inflight, &task.id);
    assert_eq!(
        back.step.as_deref(),
        Some("show_diff"),
        "review passed, and the type's next pipeline is the handoff again"
    );
    assert!(back.human_owned, "so it is yours again too");
    assert_eq!(
        repo.positions(),
        vec![
            "review/check_it round 0",
            "handoff/show_diff round 0",
            "review/check_it round 0",
            "handoff/show_diff round 0",
        ]
    );

    // And then approving lands it.
    ok(&store, &["approve", "--task", &task.id]);
    let done = drive(&store, &mut inflight, &task.id);
    assert_eq!(done.status, Status::Finished);
}

#[test]
fn a_task_that_is_not_waiting_for_you_says_so() {
    let store = Store::new();
    let repo = handed_repo();
    let root = repo.root().to_string_lossy().to_string();
    let task = store.task_in(&root, "feature", "not there yet");

    let out = shep(&store, &["approve", "--task", &task.id]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not waiting for you"), "got {stderr}");
    assert!(
        check::for_task(&store.conn(), &task.id)
            .expect("checks")
            .is_empty(),
        "and nothing was recorded"
    );
}

#[test]
fn a_pipeline_the_type_does_not_run_is_refused() {
    let store = Store::new();
    let repo = handed_repo();
    let (task, _inflight) = handed_over(&store, &repo);

    let out = shep(&store, &["run", "nonsense", "--task", &task.id]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("review → handoff → integrate"),
        "got {stderr}"
    );
}

#[test]
fn approving_from_the_pane_the_diff_is_in_needs_no_arguments() {
    let store = Store::new();
    let repo = handed_repo();
    let (task, mut inflight) = handed_over(&store, &repo);

    // `show_diff.sh` binds the pane it puts the diff in, so that pane is a pane
    // where `shep approve` resolves its own task.
    let out = std::process::Command::new(SHEP)
        .arg("approve")
        .env("SHEP_DB", store.path())
        .env("HERDR_PANE_ID", "wH:p1")
        .env_remove("SHEP_TASK_ID")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run shep");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let done = drive(&store, &mut inflight, &task.id);
    assert_eq!(done.status, Status::Finished);
    let conn = store.conn();
    assert!(
        check::for_task(&conn, &task.id)
            .expect("checks")
            .iter()
            .any(|c| c.conclusion == check::Conclusion::Pass),
        "and the approval is on the record"
    );
    let _ = Path::new("");
}

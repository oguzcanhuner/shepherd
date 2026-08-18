//! M5 acceptance: a step hands work to an agent in a pane, and the task advances
//! on its own when that agent stops.
//!
//! No Herdr session anywhere in here. The edge Herdr provides is a row in
//! `raw_event` (M2), so these tests write the payloads Herdr writes and drive the
//! supervisor over them — which is also the only way to test the endings that are
//! awkward to stage for real, like a workspace closing.

mod common;

use common::{Store, deferred_repo};
use shepherd::db::check::{Conclusion, NewCheck};
use shepherd::db::task::{Status, Task};
use shepherd::db::{self, check, event, meta, pane, raw_event, task};
use shepherd::engine;
use shepherd::supervisor::{self, Inflight};
use std::path::Path;
use std::time::{Duration, Instant};

const SHEP: &str = env!("CARGO_BIN_EXE_shep");

// ------------------------------------------------------------ the payloads
// Shapes as observed (tests/forward.rs) and as the schema describes
// (herdr-findings §5.2).

fn agent_status(pane: &str, status: &str) -> String {
    format!(
        r#"{{"event":"pane_agent_status_changed","data":{{"type":"pane_agent_status_changed","pane_id":"{pane}","workspace_id":"wZ","agent_status":"{status}","agent":"claude"}}}}"#
    )
}

fn pane_gone(kind: &str, pane: &str) -> String {
    format!(
        r#"{{"event":"pane_{kind}","data":{{"type":"pane_{kind}","pane_id":"{pane}","workspace_id":"wZ"}}}}"#
    )
}

fn workspace_closed(workspace: &str) -> String {
    format!(
        r#"{{"event":"workspace_closed","data":{{"type":"workspace_closed","workspace_id":"{workspace}","workspace":{{"workspace_id":"{workspace}","label":"t","pane_count":1}}}}}}"#
    )
}

// ------------------------------------------------------------------ driving

/// Say what Herdr would have said, exactly as `hooks/forward.sh` does.
fn herdr_said(store: &Store, body: &str) {
    let conn = store.conn();
    raw_event::append(&conn, body).expect("append raw event");
}

fn tick(store: &Store, inflight: &mut Inflight) -> supervisor::Tick {
    let mut conn = store.conn();
    supervisor::tick(&mut conn, store.path(), inflight).expect("tick")
}

/// Tick until the task stops changing, then hand back the row.
fn settle(store: &Store, task_id: &str, inflight: &mut Inflight) -> Task {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = None;
    loop {
        tick(store, inflight);
        let conn = store.conn();
        let task = task::require(&conn, task_id).expect("task");
        let here = (
            task.status,
            task.pipeline.clone(),
            task.step.clone(),
            task.round,
        );
        if inflight.is_empty() && last.as_ref() == Some(&here) {
            return task;
        }
        last = Some(here);
        assert!(Instant::now() < deadline, "task never settled: {task:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Start a task and run it up to the point where it is waiting on its agent.
fn awaiting(store: &Store, repo: &common::Repo, kind: &str) -> (Task, Inflight) {
    let root = repo.root().to_string_lossy().to_string();
    let created = store.task_in(&root, kind, "make it so");
    let mut inflight = Inflight::default();
    let task = settle(store, &created.id, &mut inflight);
    assert_eq!(
        task.status,
        Status::Running,
        "a task waiting on an agent stays running"
    );
    assert_eq!(task.step.as_deref(), Some("launch"));
    let kinds = kinds_of(store, &task.id);
    assert!(
        kinds.contains(&"task.step_awaiting".to_string()),
        "the promise is on the record: {kinds:?}"
    );
    (task, inflight)
}

fn kinds_of(store: &Store, task_id: &str) -> Vec<String> {
    let conn = store.conn();
    event::for_task(&conn, task_id)
        .expect("events")
        .into_iter()
        .map(|e| e.kind)
        .collect()
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

/// A verdict about the step a task is sitting on, written the way a linter or an
/// agent would leave it. Inserted directly: sha stamping has a test of its own,
/// and these tests are about what a check *means*.
fn check_for(store: &Store, task: &Task, conclusion: Conclusion) -> String {
    let conn = store.conn();
    check::insert(
        &conn,
        &NewCheck {
            task_id: task.id.clone(),
            pipeline: task.pipeline.clone(),
            step: task.step.clone(),
            round: Some(task.round),
            author: "claude".to_string(),
            sha: "deadbeefdeadbeef".to_string(),
            conclusion,
            body: Some("what I did".to_string()),
        },
    )
    .expect("insert check")
    .id
}

// -------------------------------------------------------------- the milestone

#[test]
fn an_agent_that_works_and_stops_carries_the_task_on() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    let (task, mut inflight) = awaiting(&store, &repo, "watched");

    // The step bound its pane, and said where the work is.
    let conn = store.conn();
    assert_eq!(
        pane::for_task(&conn, &task.id).expect("pane").as_deref(),
        Some(format!("wZ:{}", task.id).as_str())
    );
    assert_eq!(task.workspace_id.as_deref(), Some("wZ"));
    assert_eq!(task.branch.as_deref(), Some(&*format!("shep/{}", task.id)));
    drop(conn);

    let check = check_for(&store, &task, Conclusion::Pass);
    let pane = format!("wZ:{}", task.id);
    herdr_said(&store, &agent_status(&pane, "working"));
    herdr_said(&store, &agent_status(&pane, "done"));

    let finished = settle(&store, &task.id, &mut inflight);
    assert_eq!(
        finished.status,
        Status::Finished,
        "the agent stopped, review ran, and nobody was asked anything"
    );
    // The synchronous pipeline after it ran, in the worktree.
    assert_eq!(repo.order(), vec!["verify"]);

    // What resolved it, and on what evidence, is in the trace.
    let conn = store.conn();
    // Two step_finished events name `launch`: the `started` promise, and the
    // answer that redeemed it.
    let note = event::for_task(&conn, &task.id)
        .expect("events")
        .into_iter()
        .filter(|e| e.kind == "task.step_finished")
        .filter_map(|e| e.payload)
        .find(|p| p["step"] == "launch" && p["outcome"] == "pass")
        .map(|p| p["note"].as_str().unwrap_or_default().to_string())
        .expect("a note on the resolution");
    assert!(note.contains("went done"), "got {note}");
    assert!(note.contains(&check), "the note names the check: {note}");
}

#[test]
fn the_first_idle_of_a_fresh_agent_resolves_nothing() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    let (task, mut inflight) = awaiting(&store, &repo, "watched");
    check_for(&store, &task, Conclusion::Pass);

    // `herdr agent start` returns once the agent is ready for input, and that is
    // a status change in its own right. Without a remembered `working` this would
    // resolve every deferred step the moment it started.
    let pane = format!("wZ:{}", task.id);
    herdr_said(&store, &agent_status(&pane, "idle"));
    let after = settle(&store, &task.id, &mut inflight);

    assert_eq!(after.status, Status::Running);
    assert_eq!(after.step.as_deref(), Some("launch"));
    assert!(repo.order().is_empty(), "nothing after it has run");
    // The status was still noticed: it is what the next edge is measured from.
    let conn = store.conn();
    assert_eq!(
        pane::last_status(&conn, &pane).expect("status").as_deref(),
        Some("idle")
    );
}

#[test]
fn an_unknown_status_is_not_a_completion() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    let (task, mut inflight) = awaiting(&store, &repo, "watched");
    check_for(&store, &task, Conclusion::Pass);
    let pane = format!("wZ:{}", task.id);

    herdr_said(&store, &agent_status(&pane, "working"));
    herdr_said(&store, &agent_status(&pane, "unknown"));
    let after = settle(&store, &task.id, &mut inflight);

    assert_eq!(
        after.step.as_deref(),
        Some("launch"),
        "`unknown` does not prove completion (herdr-findings §5.1)"
    );
}

#[test]
fn an_agent_that_stops_without_a_check_parks_the_task() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    let (task, mut inflight) = awaiting(&store, &repo, "watched");
    let pane = format!("wZ:{}", task.id);

    herdr_said(&store, &agent_status(&pane, "working"));
    herdr_said(&store, &agent_status(&pane, "done"));
    let parked = settle(&store, &task.id, &mut inflight);

    assert_eq!(
        parked.status,
        Status::Parked,
        "an agent that leaves no check may have crashed or run out of turns"
    );
    let reason = park_reason(&store, &task.id);
    assert!(reason.contains("leaving no check"), "got {reason}");
    assert!(repo.order().is_empty(), "and nothing downstream ran");
}

#[test]
fn a_failed_check_is_the_steps_verdict() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    let (task, mut inflight) = awaiting(&store, &repo, "watched");
    check_for(&store, &task, Conclusion::Fail);
    let pane = format!("wZ:{}", task.id);

    herdr_said(&store, &agent_status(&pane, "working"));
    herdr_said(&store, &agent_status(&pane, "done"));
    let parked = settle(&store, &task.id, &mut inflight);

    // `implement` has no on_fail, so a rejection has nowhere to go until M6.
    assert_eq!(parked.status, Status::Parked);
    let reason = park_reason(&store, &task.id);
    assert!(reason.contains("rejected"), "got {reason}");
}

#[test]
fn a_pane_that_exits_resolves_the_step_too() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    let (task, mut inflight) = awaiting(&store, &repo, "watched");
    check_for(&store, &task, Conclusion::Pass);
    let pane = format!("wZ:{}", task.id);

    // No status edge at all: an agent that quit outright never reports `done`.
    herdr_said(&store, &pane_gone("exited", &pane));
    let finished = settle(&store, &task.id, &mut inflight);

    assert_eq!(finished.status, Status::Finished);
    let conn = store.conn();
    assert_eq!(
        pane::last_status(&conn, &pane).expect("status"),
        None,
        "a pane that has gone away has no agent status"
    );
}

#[test]
fn a_closed_workspace_resolves_the_task_it_took_with_it() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    let (task, mut inflight) = awaiting(&store, &repo, "watched");
    check_for(&store, &task, Conclusion::Pass);

    // Closing a workspace fires no pane events at all, and the payload
    // has no pane id — only the workspace the task recorded when it bound.
    herdr_said(&store, &workspace_closed("wZ"));
    let finished = settle(&store, &task.id, &mut inflight);

    assert_eq!(finished.status, Status::Finished);
}

#[test]
fn nothing_but_a_person_resolves_a_handoff() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    let (task, mut inflight) = awaiting(&store, &repo, "handed");
    check_for(&store, &task, Conclusion::Pass);
    let pane = format!("wZ:{}", task.id);

    // You are talking to the agent, so it goes working → done as often as you
    // like. `await = "human"` means none of that moves the state machine.
    for status in ["working", "done", "working", "done"] {
        herdr_said(&store, &agent_status(&pane, status));
    }
    let after = settle(&store, &task.id, &mut inflight);

    assert_eq!(after.status, Status::Running);
    assert_eq!(after.step.as_deref(), Some("launch"));
}

#[test]
fn events_about_panes_nobody_owns_are_read_and_dropped() {
    let store = Store::new();
    let mut conn = store.conn();
    herdr_said(&store, &agent_status("wOther:p9", "working"));
    herdr_said(&store, &agent_status("wOther:p9", "done"));
    herdr_said(&store, &pane_gone("closed", "wOther:p9"));

    let drained = engine::drain(&mut conn, 100).expect("drain");
    assert_eq!(drained.consumed, 3);
    assert_eq!(drained.resolved, 0);
    assert_eq!(
        pane::last_status(&conn, "wOther:p9").expect("status"),
        None,
        "only panes a task is working in are worth remembering"
    );
}

#[test]
fn an_event_is_acted_on_once() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    let (task, mut inflight) = awaiting(&store, &repo, "watched");
    let pane = format!("wZ:{}", task.id);
    herdr_said(&store, &agent_status(&pane, "working"));

    let mut conn = store.conn();
    let first = engine::drain(&mut conn, 100).expect("drain");
    assert_eq!(first.consumed, 1);
    let cursor = meta::raw_cursor(&conn).expect("cursor");
    assert_eq!(cursor, first.cursor);

    let second = engine::drain(&mut conn, 100).expect("drain again");
    assert_eq!(second.consumed, 0, "the cursor is this reader's place");
    assert_eq!(second.cursor, cursor);
    drop(conn);
    let _ = settle(&store, &task.id, &mut inflight);
}

#[test]
fn a_supervisor_that_was_down_still_sees_the_agent_finish() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    let (task, mut inflight) = awaiting(&store, &repo, "watched");
    check_for(&store, &task, Conclusion::Pass);
    let pane = format!("wZ:{}", task.id);

    // Everything the agent did happened while nothing was polling: the hook only
    // appends, so the log is still there to be read.
    herdr_said(&store, &agent_status(&pane, "working"));
    herdr_said(&store, &agent_status(&pane, "done"));
    let mut conn = store.conn();
    assert!(
        engine::recover_orphans(&mut conn)
            .expect("recover")
            .is_empty(),
        "a task waiting on an agent is not an orphaned step"
    );
    drop(conn);

    let finished = settle(&store, &task.id, &mut inflight);
    assert_eq!(finished.status, Status::Finished);
}

#[test]
fn a_synchronous_step_is_recovered_even_with_a_pane_bound() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    let root = repo.root().to_string_lossy().to_string();
    let task = store.task_in(&root, "watched", "left mid-lint");
    let mut conn = store.conn();

    // As a supervisor that died during `after/verify` would leave it: still
    // holding the agent pane from `implement`, because `shep context` has to keep
    // working in that pane for the life of the task.
    engine::transition(&mut conn, &task.id, |_| {
        Ok(engine::Decision::apply(
            db::task::TaskPatch::new()
                .status(Status::Running)
                .pipeline(Some("after"))
                .step(Some("verify")),
        ))
    })
    .expect("claim");
    pane::bind(&conn, "wZ:p1", &task.id).expect("bind");

    let recovered = engine::recover_orphans(&mut conn).expect("recover");
    assert_eq!(
        recovered,
        vec![task.id.clone()],
        "what a step is waiting for is what its pipeline's await says, not \
         whether a pane happens to be bound"
    );
}

#[test]
fn pausing_holds_the_events_as_well_as_the_work() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    let (task, mut inflight) = awaiting(&store, &repo, "watched");
    check_for(&store, &task, Conclusion::Pass);
    let pane = format!("wZ:{}", task.id);

    let conn = store.conn();
    meta::set_paused(&conn, true).expect("pause");
    drop(conn);
    herdr_said(&store, &agent_status(&pane, "working"));
    herdr_said(&store, &agent_status(&pane, "done"));

    let held = tick(&store, &mut inflight);
    assert!(held.paused);
    assert_eq!(held.events, 0, "the log waits; the cursor has not moved");
    assert_eq!(
        task::require(&store.conn(), &task.id).expect("task").step,
        Some("launch".to_string())
    );

    let conn = store.conn();
    meta::set_paused(&conn, false).expect("resume");
    drop(conn);
    let finished = settle(&store, &task.id, &mut inflight);
    assert_eq!(finished.status, Status::Finished);
}

// ------------------------------------------------- what an agent in a pane has

/// Run the real binary, the way an agent in a pane would: no `--db`, no
/// `--task`, only the environment Herdr and the split gave it.
fn in_pane(store: &Store, env: &[(&str, &str)], args: &[&str]) -> std::process::Output {
    let mut cmd = std::process::Command::new(SHEP);
    cmd.args(args)
        .env("SHEP_DB", store.path())
        .env_remove("SHEP_TASK_ID")
        .env_remove("HERDR_PANE_ID")
        .env_remove("SHEP_PIPELINE")
        .env_remove("SHEP_STEP")
        .env_remove("SHEP_ROUND")
        .stdin(std::process::Stdio::null());
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("run shep")
}

fn stdout_of(out: &std::process::Output) -> String {
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn context_resolves_its_own_task_from_the_pane_it_is_running_in() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    let (task, _inflight) = awaiting(&store, &repo, "watched");
    let pane = format!("wZ:{}", task.id);

    // All an agent has is $HERDR_PANE_ID (herdr-findings §2), which is the whole
    // reason `pane_task` exists.
    let out = stdout_of(&in_pane(&store, &[("HERDR_PANE_ID", &pane)], &["context"]));
    assert!(out.contains("make it so"), "the brief is the point: {out}");
    assert!(out.contains(&task.id), "got {out}");
    assert!(out.contains("implement/launch"), "got {out}");

    // A step script has $SHEP_TASK_ID instead, and gets the same answer.
    let json = stdout_of(&in_pane(
        &store,
        &[("SHEP_TASK_ID", &task.id)],
        &["context", "--json"],
    ));
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("json");
    assert_eq!(parsed["brief"], "make it so");
    assert_eq!(parsed["pane"], pane);
}

#[test]
fn context_with_nothing_to_go_on_says_so() {
    let store = Store::new();
    let out = in_pane(&store, &[], &["context"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("SHEP_TASK_ID"), "got {stderr}");
    assert!(stderr.contains("HERDR_PANE_ID"), "got {stderr}");

    // A pane Herdr knows about but nothing has bound is a different problem, and
    // says which one it is.
    let out = in_pane(&store, &[("HERDR_PANE_ID", "wQ:p1")], &["context"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("bind-pane"), "got {stderr}");
}

#[test]
fn a_submitted_check_is_stamped_with_the_commit_it_judged() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    repo.git_init();
    let (task, _inflight) = awaiting(&store, &repo, "watched");
    let pane = format!("wZ:{}", task.id);

    let id = stdout_of(&in_pane(
        &store,
        &[("HERDR_PANE_ID", &pane)],
        &["check", "submit", "--pass"],
    ))
    .trim()
    .to_string();

    let conn = store.conn();
    let written = check::get(&conn, &id).expect("get").expect("the check");
    assert_eq!(
        written.sha,
        repo.head(),
        "the submitter never supplies this"
    );
    assert_eq!(written.conclusion, Conclusion::Pass);
    // Nothing in that pane's environment said where the task was, so the row did.
    assert_eq!(written.pipeline.as_deref(), Some("implement"));
    assert_eq!(written.step.as_deref(), Some("launch"));
    assert_eq!(written.round, Some(0));
    assert_eq!(
        written.author, "launch",
        "unattributed means the step that is running"
    );
    assert!(
        kinds_of(&store, &task.id).contains(&"task.check_submitted".to_string()),
        "and it is on the record"
    );
}

#[test]
fn a_step_script_says_which_position_it_is_judging() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    repo.git_init();
    let (task, _inflight) = awaiting(&store, &repo, "watched");

    let id = stdout_of(&in_pane(
        &store,
        &[
            ("SHEP_TASK_ID", &task.id),
            ("SHEP_PIPELINE", "after"),
            ("SHEP_STEP", "verify"),
            ("SHEP_ROUND", "2"),
        ],
        &["check", "submit", "--fail", "--author", "clippy"],
    ))
    .trim()
    .to_string();

    let conn = store.conn();
    let written = check::get(&conn, &id).expect("get").expect("the check");
    assert_eq!(written.pipeline.as_deref(), Some("after"));
    assert_eq!(written.step.as_deref(), Some("verify"));
    assert_eq!(written.round, Some(2));
    assert_eq!(written.author, "clippy");
    assert_eq!(written.conclusion, Conclusion::Fail);
}

#[test]
fn a_check_reads_back_by_its_own_name() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    repo.git_init();
    let (task, _inflight) = awaiting(&store, &repo, "watched");
    let id = check_for(&store, &task, Conclusion::Pass);

    let out = stdout_of(&in_pane(&store, &[], &["read", &id]));
    assert!(out.contains(&id), "got {out}");
    assert!(
        out.contains("what I did"),
        "the body is the artefact: {out}"
    );
    assert!(out.contains("claude"), "got {out}");
}

#[test]
fn one_says_which_verdict_it_means() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    repo.git_init();
    let (task, _inflight) = awaiting(&store, &repo, "watched");

    let out = in_pane(
        &store,
        &[("SHEP_TASK_ID", &task.id)],
        &["check", "submit", "--pass", "--fail"],
    );
    assert!(!out.status.success(), "--pass --fail is not a verdict");

    let out = in_pane(&store, &[("SHEP_TASK_ID", &task.id)], &["check", "submit"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--pass"), "got {stderr}");
}

#[test]
fn a_check_carries_the_body_it_was_given() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    repo.git_init();
    let (task, _inflight) = awaiting(&store, &repo, "watched");

    // Body on stdin, which is how an agent hands over a paragraph.
    let mut child = std::process::Command::new(SHEP)
        .args(["--db"])
        .arg(store.path())
        .args(["check", "submit", "--pass", "--task", &task.id])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(b"rewrote the parser; the old one could not see a comment\n")
            .expect("write");
    }
    let out = child.wait_with_output().expect("wait");
    let id = stdout_of(&out).trim().to_string();

    let conn = store.conn();
    let written = check::get(&conn, &id).expect("get").expect("check");
    assert_eq!(
        written.body.as_deref(),
        Some("rewrote the parser; the old one could not see a comment")
    );
}

#[test]
fn binding_a_pane_twice_keeps_the_worktree_it_already_had() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    let (task, _inflight) = awaiting(&store, &repo, "watched");

    // What a retry does: bind the pane again, saying nothing about the worktree.
    let out = in_pane(
        &store,
        &[("SHEP_TASK_ID", &task.id)],
        &["bind-pane", "wZ:p9"],
    );
    stdout_of(&out);

    let conn = store.conn();
    let after = task::require(&conn, &task.id).expect("task");
    assert_eq!(
        after.worktree.as_deref(),
        Some(repo.root().to_string_lossy().as_ref()),
        "absent means leave it alone"
    );
    assert_eq!(
        pane::for_task(&conn, &task.id).expect("pane").as_deref(),
        Some("wZ:p9"),
        "and the newest pane is the one it is working in"
    );
    assert_eq!(
        pane::all_for_task(&conn, &task.id).expect("panes").len(),
        2,
        "without forgetting the one before it"
    );
}

#[test]
fn a_finished_task_gets_no_new_panes() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    let root = repo.root().to_string_lossy().to_string();
    let task = store.task_in(&root, "watched", "over and done with");
    let mut conn = store.conn();
    engine::cancel(&mut conn, &task.id, Some("changed my mind".into())).expect("cancel");
    drop(conn);

    let out = in_pane(
        &store,
        &[("SHEP_TASK_ID", &task.id)],
        &["bind-pane", "wZ:p1"],
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cancelled"), "got {stderr}");
    assert_eq!(
        pane::for_task(&store.conn(), &task.id).expect("pane"),
        None,
        "and nothing was bound"
    );
}

#[test]
fn the_store_is_reachable_from_a_step_by_the_environment_alone() {
    let store = Store::new();
    let repo = deferred_repo(SHEP);
    let (task, _inflight) = awaiting(&store, &repo, "watched");

    // `launch.sh` reached back in with no --db and no --task: everything it needed
    // was in the environment the contract promises.
    let conn = store.conn();
    let bound = pane::for_task(&conn, &task.id).expect("pane");
    assert_eq!(bound, Some(format!("wZ:{}", task.id)));

    // And that environment names a `shep` to call, whatever is on $PATH.
    let spec = engine::StepSpec::resolve(
        &shepherd::config::Policy::load(Path::new(&task.repo)).expect("policy"),
        &task,
        "implement",
        "launch",
        0,
        store.path(),
        bound,
    )
    .expect("resolve");
    let env: std::collections::BTreeMap<_, _> = engine::environment(&spec).into_iter().collect();
    assert!(env.contains_key("SHEP_BIN"), "got {env:?}");
    assert_eq!(env["SHEP_DB"], store.path().display().to_string());
    assert_eq!(env["SHEP_PANE"], format!("wZ:{}", task.id));
}

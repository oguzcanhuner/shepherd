//! End to end through the real binary: `shep status` must be right with the
//! supervisor up, stopped cleanly, and killed outright (M1), and the commands
//! that read policy must behave the way an agent needs them to (M3).

mod common;

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SHEP: &str = env!("CARGO_BIN_EXE_shep");

struct Run {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

fn shep(db: &Path, args: &[&str]) -> Run {
    let out = Command::new(SHEP)
        .arg("--db")
        .arg(db)
        .args(args)
        // The suite may itself run inside a Herdr pane; a test must never
        // revive a real supervisor over a temp store.
        .env("SHEP_NO_REVIVE", "1")
        .env_remove("HERDR_PANE_ID")
        .output()
        .expect("run shep");
    Run {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

fn ok(db: &Path, args: &[&str]) -> String {
    let run = shep(db, args);
    assert!(
        run.status.success(),
        "shep {args:?} failed: {}{}",
        run.stdout,
        run.stderr
    );
    run.stdout
}

fn status_json(db: &Path) -> serde_json::Value {
    serde_json::from_str(&ok(db, &["status", "--json"])).expect("status json")
}

/// Wait for the supervisor to report a state, so the test never races the poll.
fn wait_for_supervisor(db: &Path, want: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = status_json(db);
        if status["supervisor"] == want {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "supervisor never became {want}: {status}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn spawn_supervisor(db: &Path) -> Child {
    Command::new(SHEP)
        .arg("--db")
        .arg(db)
        .args(["supervise", "--poll-ms", "20"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn supervisor")
}

fn signal(child: &Child, sig: i32) {
    assert_eq!(
        unsafe { libc::kill(child.id() as i32, sig) },
        0,
        "signalling the supervisor"
    );
}

/// Wait for a task to reach a status, so the test never races the poll.
fn wait_for_task(db: &Path, task: &str, want: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let got: serde_json::Value =
            serde_json::from_str(&ok(db, &["get", task, "--json"])).expect("get json");
        if got["status"] == want {
            return got;
        }
        assert!(
            Instant::now() < deadline,
            "task {task} never became {want}: {got}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn a_parked_task_explains_itself_and_can_be_retried() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("shep.db");
    let repo = common::scripted_repo();
    let root = repo.root().to_string_lossy().to_string();
    repo.says(r#"{"outcome":"error","note":"the linter fell over"}"#);

    let task = ok(
        &db,
        &[
            "create",
            "--type",
            "simple",
            "--repo",
            &root,
            "break please",
        ],
    )
    .trim()
    .to_string();

    let mut child = spawn_supervisor(&db);
    wait_for_supervisor(&db, "running");
    wait_for_task(&db, &task, "parked");

    // The trace is the answer to "why is this stuck?".
    let trace = ok(&db, &["trace", &task]);
    assert!(trace.contains("task.step_started"), "got {trace}");
    assert!(trace.contains("→ error"), "got {trace}");
    assert!(trace.contains("the linter fell over"), "got {trace}");
    assert!(trace.contains("task.parked"), "got {trace}");
    // A consequence is drawn under what caused it.
    assert!(trace.contains("└─ task.parked"), "got {trace}");

    // `shep get` says where it stopped, so a retry is a considered thing to do.
    let detail = ok(&db, &["get", &task]);
    assert!(detail.contains("parked"), "got {detail}");
    assert!(detail.contains("outcome"), "got {detail}");

    // Fix the cause, retry, and it runs to rest.
    repo.says(r#"{"outcome":"pass"}"#);
    assert!(ok(&db, &["retry", &task]).contains("queued again"));
    wait_for_task(&db, &task, "resting");

    let trace = ok(&db, &["trace", &task]);
    assert!(trace.contains("task.resumed"), "got {trace}");
    assert!(trace.contains("task.rested"), "got {trace}");

    signal(&child, libc::SIGTERM);
    child.wait().expect("wait");

    // And a rested task is not retryable.
    let run = shep(&db, &["retry", &task]);
    assert!(!run.status.success());
    assert!(
        run.stderr.contains("only a parked task"),
        "got {}",
        run.stderr
    );
}

#[test]
fn a_task_can_be_cancelled() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("shep.db");
    let repo = common::scripted_repo();
    let task = ok(
        &db,
        &[
            "create",
            "--type",
            "simple",
            "--repo",
            &repo.root().to_string_lossy(),
            "never mind this one",
        ],
    )
    .trim()
    .to_string();

    // No supervisor running, so nothing has started it: cancelling is just a
    // transaction.
    assert!(ok(&db, &["cancel", &task, "--reason", "changed my mind"]).contains("cancelled"));

    let detail: serde_json::Value =
        serde_json::from_str(&ok(&db, &["get", &task, "--json"])).expect("json");
    assert_eq!(detail["status"], "cancelled");

    // Cancelled tasks are out of the way by default, and `--all` still shows them.
    assert!(ok(&db, &["ps"]).contains("no open tasks"));
    assert!(ok(&db, &["ps", "--all"]).contains(&task));

    let trace = ok(&db, &["trace", &task]);
    assert!(trace.contains("changed my mind"), "got {trace}");
}

#[test]
fn status_on_a_store_that_does_not_exist_yet() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("nothing-here.db");

    let human = ok(&db, &["status"]);
    assert!(human.contains("absent"), "got {human}");
    assert!(human.contains("supervisor  down"), "got {human}");

    let json = status_json(&db);
    assert_eq!(json["exists"], false);
    assert_eq!(json["supervisor"], "down");
    assert!(!db.exists(), "a read must not mint a store");
}

#[test]
fn create_then_list_then_pause() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("shep.db");
    let repo = policy_repo();
    let root = repo.root().to_string_lossy().to_string();

    let id = ok(
        &db,
        &[
            "create",
            "--type",
            "feature",
            "--repo",
            &root,
            "add a widget",
        ],
    )
    .trim()
    .to_string();
    assert_eq!(
        id, "t-1",
        "stdout is the id alone, for TASK=$(shep create ...)"
    );

    let table = ok(&db, &["ps"]);
    assert!(table.contains("t-1"), "got {table}");
    assert!(table.contains("queued"), "got {table}");
    assert!(table.contains("add a widget"), "got {table}");

    let rows: serde_json::Value = serde_json::from_str(&ok(&db, &["ps", "--json"])).expect("json");
    assert_eq!(rows[0]["id"], "t-1");
    assert_eq!(rows[0]["type"], "feature");
    assert_eq!(rows[0]["status"], "queued");
    assert_eq!(rows[0]["brief"], "add a widget");

    assert_eq!(status_json(&db)["tasks"]["queued"], 1);

    assert!(ok(&db, &["pause"]).contains("paused"));
    assert_eq!(status_json(&db)["paused"], true);
    assert!(ok(&db, &["resume"]).contains("resumed"));
    assert_eq!(status_json(&db)["paused"], false);
}

/// A repo with the example config, for the commands that read policy.
fn policy_repo() -> common::Repo {
    let repo = common::Repo::new();
    for step in [
        "code",
        "lint",
        "test",
        "agent_review",
        "fix",
        "show_diff",
        "integrate",
    ] {
        repo.script(step);
    }
    repo.write(
        r#"
[pipeline.implement]
steps = [{ run = "code", await = "agent_stopped" }]

[pipeline.review]
steps        = ["lint", "test", "agent_review"]
on_fail      = "fix"
max_rounds   = 3
on_exhausted = "reject"

[pipeline.handoff]
steps = ["show_diff"]

[pipeline.integrate]
steps = ["integrate"]

[type.feature]
description = "Normal change. Reviewed, then shown to you."
pipelines   = ["implement", "review", "handoff"]

[type.hotfix]
description = "Urgent production fix. No review, no handoff."
pipelines   = ["implement", "integrate"]
"#,
    );
    repo
}

#[test]
fn types_prints_the_menu_an_agent_chooses_from() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("shep.db");
    let repo = policy_repo();

    let out = ok(&db, &["types", "--repo", &repo.root().to_string_lossy()]);
    assert!(out.contains("feature"), "got {out}");
    assert!(out.contains("Normal change"), "got {out}");
    assert!(out.contains("hotfix"), "got {out}");
    assert!(out.contains("implement"), "got {out}");

    let json: serde_json::Value = serde_json::from_str(&ok(
        &db,
        &["types", "--repo", &repo.root().to_string_lossy(), "--json"],
    ))
    .expect("json");
    assert_eq!(json[0]["type"], "feature");
    assert_eq!(json[0]["pipelines"][0], "implement");
}

#[test]
fn validate_reports_a_good_config_and_a_bad_one() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("shep.db");
    let repo = policy_repo();
    let root = repo.root().to_string_lossy().to_string();

    let out = ok(&db, &["validate", "--repo", &root]);
    assert!(out.contains("is valid"), "got {out}");
    // It shows what each step resolved to, since the filename is the registration.
    assert!(out.contains("lint.sh"), "got {out}");
    assert!(out.contains("await agent_stopped"), "got {out}");
    assert!(out.contains("on_fail → fix"), "got {out}");

    let json: serde_json::Value =
        serde_json::from_str(&ok(&db, &["validate", "--repo", &root, "--json"])).expect("json");
    assert_eq!(json["valid"], true);

    // Now break it.
    repo.write("[pipeline.review]\nsteps = [\"nope\"]\n");
    let run = shep(&db, &["validate", "--repo", &root]);
    assert!(!run.status.success(), "a broken config must exit non-zero");
    assert!(run.stderr.contains("nope"), "got {}", run.stderr);
    assert!(
        run.stderr.contains("no types defined"),
        "got {}",
        run.stderr
    );

    let run = shep(&db, &["validate", "--repo", &root, "--json"]);
    assert!(!run.status.success());
    let json: serde_json::Value = serde_json::from_str(&run.stdout).expect("json");
    assert_eq!(json["valid"], false);
    assert!(json["problems"].as_array().expect("array").len() >= 2);
}

#[test]
fn an_invalid_type_returns_the_menu_and_creates_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("shep.db");
    let repo = policy_repo();
    let root = repo.root().to_string_lossy().to_string();

    let run = shep(
        &db,
        &[
            "create",
            "--type",
            "refactor",
            "--repo",
            &root,
            "do a thing",
        ],
    );
    assert!(!run.status.success());
    assert!(
        run.stderr.contains("unknown type \"refactor\""),
        "got {}",
        run.stderr
    );
    // The agent that guessed wrong is the one that has to choose again.
    assert!(run.stderr.contains("feature"), "got {}", run.stderr);
    assert!(
        run.stderr.contains("Urgent production fix"),
        "got {}",
        run.stderr
    );

    // And no task was queued that could never have run.
    let rows: serde_json::Value = serde_json::from_str(&ok(&db, &["ps", "--json"])).expect("json");
    assert_eq!(rows.as_array().expect("array").len(), 0);

    // The valid type does create one.
    let id = ok(
        &db,
        &["create", "--type", "feature", "--repo", &root, "do a thing"],
    );
    assert_eq!(id.trim(), "t-1");
}

#[test]
fn creating_in_a_repo_with_no_policy_says_so() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("shep.db");
    let bare = tempfile::tempdir().expect("temp dir");

    let run = shep(
        &db,
        &[
            "create",
            "--type",
            "feature",
            "--repo",
            &bare.path().to_string_lossy(),
            "x",
        ],
    );
    assert!(!run.status.success());
    assert!(
        run.stderr.contains(".shep/config.toml"),
        "got {}",
        run.stderr
    );
}

#[test]
fn a_brief_is_required() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("shep.db");
    let run = shep(&db, &["create", "--type", "feature"]);
    assert!(!run.status.success());
    assert!(run.stderr.contains("BRIEF"), "got {}", run.stderr);
}

#[test]
fn status_tracks_a_supervisor_up_then_stopped_cleanly() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("shep.db");

    let mut child = spawn_supervisor(&db);
    let up = wait_for_supervisor(&db, "running");
    assert_eq!(up["healthy"], true);
    assert_eq!(up["pid"], child.id());

    // A second supervisor must refuse rather than double-advance the same tasks.
    let second = shep(&db, &["supervise"]);
    assert!(!second.status.success());
    assert!(
        second.stderr.contains("already running"),
        "got {}",
        second.stderr
    );

    signal(&child, libc::SIGTERM);
    assert!(child.wait().expect("wait").success());

    let down = status_json(&db);
    assert_eq!(
        down["supervisor"], "down",
        "a clean stop clears the heartbeat"
    );
    assert_eq!(down["healthy"], false);
    assert!(Path::new(&dir.path().join("supervisor.log")).exists());
}

#[test]
fn status_notices_a_supervisor_that_was_killed_outright() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("shep.db");

    let mut child = spawn_supervisor(&db);
    wait_for_supervisor(&db, "running");

    // No chance to clean up: the heartbeat is left behind.
    signal(&child, libc::SIGKILL);
    child.wait().expect("wait");

    let dead = wait_for_supervisor(&db, "dead");
    assert_eq!(dead["healthy"], false);
    let human = ok(&db, &["status"]);
    assert!(human.contains("died without cleaning up"), "got {human}");
}

#[test]
fn a_task_created_while_the_supervisor_runs_is_seen_by_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("shep.db");

    let mut child = spawn_supervisor(&db);
    wait_for_supervisor(&db, "running");

    // No IPC: the create is a transaction, and the supervisor notices on its
    // next poll.
    let repo = policy_repo();
    ok(
        &db,
        &[
            "create",
            "--type",
            "feature",
            "--repo",
            &repo.root().to_string_lossy(),
            "seen me yet",
        ],
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    let log = dir.path().join("supervisor.log");
    loop {
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        if text.contains("queued=1") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the supervisor never saw the task. log:\n{text}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    signal(&child, libc::SIGTERM);
    child.wait().expect("wait");
}

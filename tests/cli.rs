//! End to end through the real binary: `shep status` must be right with the
//! supervisor up, stopped cleanly, and killed outright (M1).

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

    let id = ok(
        &db,
        &[
            "create",
            "--type",
            "feature",
            "--repo",
            "/tmp",
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
    // next poll (PLAN §7.4).
    ok(
        &db,
        &[
            "create",
            "--type",
            "feature",
            "--repo",
            "/tmp",
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

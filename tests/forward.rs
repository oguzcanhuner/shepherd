//! M2 acceptance: the Herdr edge. The hook appends and nothing else, so these
//! tests are about the raw event surviving verbatim.

mod common;

use shepherd::db::raw_event;
use std::path::Path;
use std::process::{Command, Stdio};

const SHEP: &str = env!("CARGO_BIN_EXE_shep");

/// A real `pane.agent_status_changed` payload, as observed.
const AGENT_DONE: &str = r#"{"event":"pane_agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"wQ:p1","workspace_id":"wQ","agent_status":"done","agent":"claude"}}"#;

/// A real `workspace.closed` payload: a workspace id and no pane id.
const WORKSPACE_CLOSED: &str = r#"{"event":"workspace_closed","data":{"type":"workspace_closed","workspace_id":"wT","workspace":{"workspace_id":"wT","label":"shep-probe4","pane_count":1}}}"#;

fn forward(db: &Path, event_json: Option<&str>, stdin: Option<&str>) -> std::process::Output {
    let mut cmd = Command::new(SHEP);
    cmd.arg("--db").arg(db).arg("forward");
    // The suite may itself run inside a Herdr pane; a test must never revive
    // a real supervisor over a temp store.
    cmd.env("SHEP_NO_REVIVE", "1").env_remove("HERDR_PANE_ID");
    match event_json {
        Some(json) => {
            cmd.env("HERDR_PLUGIN_EVENT_JSON", json);
            cmd.env("HERDR_PLUGIN_EVENT", "pane.agent_status_changed");
        }
        // A parent environment that already has these set must not leak in.
        None => {
            cmd.env_remove("HERDR_PLUGIN_EVENT_JSON");
            cmd.env_remove("HERDR_PLUGIN_EVENT");
        }
    }
    match stdin {
        Some(text) => {
            use std::io::Write;
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = cmd.spawn().expect("spawn shep forward");
            child
                .stdin
                .as_mut()
                .expect("stdin")
                .write_all(text.as_bytes())
                .expect("write stdin");
            child.wait_with_output().expect("wait")
        }
        None => cmd.stdin(Stdio::null()).output().expect("run shep forward"),
    }
}

#[test]
fn an_event_from_the_environment_is_stored_verbatim() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("shep.db");

    let out = forward(&db, Some(AGENT_DONE), None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The seq goes to stdout so `herdr plugin log list` can be read as a trace.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("raw_event 1"), "got {stdout}");

    let conn = shepherd::db::open(&db).expect("open");
    let events = raw_event::recent(&conn, 10).expect("raw events");
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].body, AGENT_DONE,
        "the store keeps what Herdr said, as it said it"
    );

    // Herdr underscores the event name inside the payload and dots it in the
    // environment; kind() reports the dotted form.
    assert_eq!(
        events[0].kind().as_deref(),
        Some("pane.agent_status_changed")
    );
    let json = events[0].json().expect("json");
    assert_eq!(json["data"]["pane_id"], "wQ:p1");
    assert_eq!(json["data"]["agent_status"], "done");
}

#[test]
fn a_workspace_close_is_forwarded_too() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("shep.db");

    assert!(forward(&db, Some(WORKSPACE_CLOSED), None).status.success());

    let conn = shepherd::db::open(&db).expect("open");
    let events = raw_event::recent(&conn, 10).expect("raw events");
    assert_eq!(events[0].kind().as_deref(), Some("workspace.closed"));
    // No pane id in this payload: the route to a task is the workspace id.
    let json = events[0].json().expect("json");
    assert_eq!(json["data"]["workspace_id"], "wT");
    assert!(json["data"]["pane_id"].is_null());
}

#[test]
fn stdin_is_accepted_so_the_edge_can_be_replayed_by_hand() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("shep.db");

    let out = forward(&db, None, Some(AGENT_DONE));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let conn = shepherd::db::open(&db).expect("open");
    assert_eq!(raw_event::count(&conn).expect("count"), 1);
}

#[test]
fn forwarding_nothing_is_an_error_that_says_why() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("shep.db");

    let out = forward(&db, None, None);
    assert!(!out.status.success(), "silence must not look like success");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("HERDR_PLUGIN_EVENT_JSON"), "got {stderr}");
}

#[test]
fn raw_reads_back_what_was_forwarded() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = dir.path().join("shep.db");

    forward(&db, Some(AGENT_DONE), None);
    forward(&db, Some(WORKSPACE_CLOSED), None);

    let out = Command::new(SHEP)
        .arg("--db")
        .arg(&db)
        .args(["raw"])
        .output()
        .expect("shep raw");
    let table = String::from_utf8_lossy(&out.stdout);
    // Oldest first, so reading top to bottom reads chronologically.
    let lines: Vec<&str> = table.lines().collect();
    assert!(
        lines[0].contains("pane.agent_status_changed"),
        "got {table}"
    );
    assert!(
        lines[0].contains("wQ:p1") && lines[0].contains("done"),
        "got {table}"
    );
    assert!(lines[1].contains("workspace.closed"), "got {table}");

    let json: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(
        &(Command::new(SHEP)
            .arg("--db")
            .arg(&db)
            .args(["raw", "--json"])
            .output()
            .expect("shep raw --json")
            .stdout),
    ))
    .expect("json");
    assert_eq!(json[0]["event"], "pane.agent_status_changed");
    assert_eq!(json[1]["body"]["data"]["workspace_id"], "wT");
}

#[test]
fn a_cursor_reads_each_event_once() {
    let store = common::Store::new();
    let conn = store.conn();

    let first = raw_event::append(&conn, AGENT_DONE).expect("append");
    let second = raw_event::append(&conn, WORKSPACE_CLOSED).expect("append");
    assert!(second > first, "seq must increase");

    // How the supervisor will consume these in M5: everything after the cursor,
    // oldest first.
    let batch = raw_event::since(&conn, 0, 10).expect("since");
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[0].seq, first);

    let batch = raw_event::since(&conn, first, 10).expect("since");
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].seq, second);

    assert!(
        raw_event::since(&conn, second, 10)
            .expect("since")
            .is_empty()
    );
}

#[test]
fn an_unparseable_body_is_still_kept() {
    let store = common::Store::new();
    let conn = store.conn();

    // If Herdr ever changes its payload, the row is still the record. Losing it
    // because we could not parse it would be worse than keeping it raw.
    raw_event::append(&conn, "not json at all").expect("append");
    let events = raw_event::recent(&conn, 1).expect("recent");
    assert_eq!(events[0].body, "not json at all");
    assert_eq!(events[0].kind(), None);
    assert_eq!(events[0].json(), None);
}

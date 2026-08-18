use crate::db::{self, event, task};
use anyhow::Result;
use std::path::Path;

/// `shep trace <task>` — what happened, and what each thing led to.
///
/// The event table is an audit trail, not a subscription bus (PLAN §1): nothing
/// reads it to make a decision, and this is what it is for.
pub fn run(db_path: &Path, task_id: &str, json: bool) -> Result<()> {
    let conn = db::open_existing(db_path)?;
    let task = task::require(&conn, task_id)?;
    let events = event::for_task(&conn, &task.id)?;

    if json {
        let rows: Vec<_> = events
            .iter()
            .map(|e| {
                serde_json::json!({
                    "seq": e.seq,
                    "ts": e.ts,
                    "type": e.kind,
                    "caused_by": e.caused_by,
                    "payload": e.payload,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "task": task.id,
                "status": task.status.as_str(),
                "events": rows,
            }))?
        );
        return Ok(());
    }

    let position = match (&task.pipeline, &task.step) {
        (Some(p), Some(s)) => format!(" at {p}/{s} round {}", task.round),
        (Some(p), None) => format!(" in {p}"),
        _ => String::new(),
    };
    println!("{}  {}  {}{position}", task.id, task.kind, task.status);
    println!("brief: {}", task.brief);
    println!("repo:  {}", task.repo);
    if let Some(pane) = db::pane::for_task(&conn, &task.id)? {
        println!("pane:  {pane}");
    }
    println!();

    let now = db::now();
    for e in &events {
        // A consequence is indented under the thing that caused it.
        let branch = if e.caused_by.is_some() { "└─ " } else { "" };
        println!(
            "{:>5}  {:>9}  {branch}{:<22} {}",
            e.seq,
            super::ago(now - e.ts),
            e.kind,
            summarise(e)
        );
    }
    Ok(())
}

/// The part of a payload worth reading in a line.
fn summarise(e: &event::Event) -> String {
    let Some(payload) = &e.payload else {
        return String::new();
    };
    let get = |key: &str| payload.get(key).and_then(|v| v.as_str()).unwrap_or("");

    let mut parts = Vec::new();
    let position = match (get("pipeline"), get("step")) {
        ("", "") => String::new(),
        ("", step) => step.to_string(),
        (pipeline, "") => pipeline.to_string(),
        (pipeline, step) => format!("{pipeline}/{step}"),
    };
    if !position.is_empty() {
        let round = payload.get("round").and_then(|v| v.as_i64()).unwrap_or(0);
        parts.push(if round > 0 {
            format!("{position} round {round}")
        } else {
            position
        });
    }
    for key in ["outcome", "await", "type", "reason", "note", "pane"] {
        let value = get(key);
        if !value.is_empty() {
            parts.push(match key {
                "outcome" => format!("→ {value}"),
                _ => value.to_string(),
            });
        }
    }
    parts.join("  ")
}

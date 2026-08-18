use crate::db::{self, raw_event};
use anyhow::Result;
use std::path::Path;

/// `shep raw` — read back what Herdr has said. The audit trail for the edge
/// itself: if a hook is not firing, this is empty and `herdr plugin log list`
/// says why.
pub fn run(db_path: &Path, limit: i64, json: bool) -> Result<()> {
    let conn = match db::open_existing(db_path) {
        Ok(conn) => conn,
        Err(crate::Error::NoStore(_)) => {
            if json {
                println!("[]");
            } else {
                println!("no raw events");
            }
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let events = raw_event::recent(&conn, limit)?;
    if json {
        let rows: Vec<_> = events
            .iter()
            .map(|e| {
                serde_json::json!({
                    "seq": e.seq,
                    "ts": e.ts,
                    "event": e.kind(),
                    "body": e.json().unwrap_or_else(|| serde_json::json!(e.body)),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if events.is_empty() {
        println!("no raw events — has the plugin been linked?");
        return Ok(());
    }

    let now = db::now();
    for e in &events {
        let kind = e.kind().unwrap_or_else(|| "unparseable".to_string());
        let pane = e
            .json()
            .and_then(|v| v["data"]["pane_id"].as_str().map(str::to_string))
            .unwrap_or_else(|| "-".to_string());
        let detail = e
            .json()
            .and_then(|v| v["data"]["agent_status"].as_str().map(str::to_string))
            .unwrap_or_default();
        println!(
            "{:<5} {:<10} {:<28} {:<8} {}",
            e.seq,
            super::ago(now - e.ts),
            kind,
            pane,
            detail
        );
    }
    Ok(())
}

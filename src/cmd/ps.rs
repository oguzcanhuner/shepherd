use crate::db::{self, now, task};
use anyhow::Result;
use std::path::Path;

/// `shep ps` — a plain aligned table. M9 refines the columns; this is enough to
/// see what the store holds.
pub fn run(db_path: &Path, all: bool, json: bool) -> Result<()> {
    let conn = match db::open_existing(db_path) {
        Ok(conn) => conn,
        Err(crate::Error::NoStore(_)) => {
            if json {
                println!("[]");
            } else {
                println!("no tasks");
            }
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let mut tasks = task::list(&conn)?;
    if !all {
        tasks.retain(|t| !t.status.is_terminal());
    }

    if json {
        let rows: Vec<_> = tasks
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "type": t.kind,
                    "status": t.status.as_str(),
                    "pipeline": t.pipeline,
                    "step": t.step,
                    "round": t.round,
                    "repo": t.repo,
                    "brief": t.brief,
                    "created": t.created,
                    "updated": t.updated,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if tasks.is_empty() {
        println!("no {}tasks", if all { "" } else { "open " });
        return Ok(());
    }

    let now = now();
    let mut rows: Vec<[String; 6]> = vec![[
        "ID".into(),
        "TYPE".into(),
        "STATUS".into(),
        "WHERE".into(),
        "UPDATED".into(),
        "BRIEF".into(),
    ]];
    for t in &tasks {
        let position = match (&t.pipeline, &t.step) {
            (Some(p), Some(s)) => format!("{p}/{s}"),
            (Some(p), None) => p.clone(),
            _ => "-".to_string(),
        };
        let position = if t.round > 0 {
            format!("{position} r{}", t.round)
        } else {
            position
        };
        rows.push([
            t.id.clone(),
            t.kind.clone(),
            t.status.to_string(),
            position,
            super::ago(now - t.updated),
            super::truncate(&t.brief, 48),
        ]);
    }

    // Last column is not padded, so a long brief cannot push the line about.
    let widths: Vec<usize> = (0..5)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    for row in &rows {
        let mut line = String::new();
        for i in 0..5 {
            line.push_str(&format!("{:<width$}  ", row[i], width = widths[i]));
        }
        line.push_str(&row[5]);
        println!("{}", line.trim_end());
    }
    Ok(())
}

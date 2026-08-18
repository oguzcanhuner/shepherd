use crate::db::{self, check, task};
use anyhow::Result;
use std::path::Path;

/// `shep get <task>` — everything the store knows about one task.
pub fn run(db_path: &Path, task_id: &str, json: bool) -> Result<()> {
    let conn = db::open_existing(db_path)?;
    let task = task::require(&conn, task_id)?;
    let pane = db::pane::for_task(&conn, &task.id)?;
    let checks = check::for_task(&conn, &task.id)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": task.id,
                "brief": task.brief,
                "type": task.kind,
                "pipeline": task.pipeline,
                "step": task.step,
                "round": task.round,
                "status": task.status.as_str(),
                "human_owned": task.human_owned,
                "repo": task.repo,
                "worktree": task.worktree,
                "branch": task.branch,
                "base": task.base,
                "workspace_id": task.workspace_id,
                "pane": pane,
                "created": task.created,
                "updated": task.updated,
                "checks": checks.iter().map(|c| serde_json::json!({
                    "id": c.id,
                    "author": c.author,
                    "conclusion": c.conclusion.as_str(),
                    "sha": c.sha,
                    "pipeline": c.pipeline,
                    "step": c.step,
                    "round": c.round,
                    "body": c.body,
                    "created": c.created,
                })).collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }

    let mut rows = vec![
        ("id", task.id.clone()),
        ("brief", task.brief.clone()),
        ("type", task.kind.clone()),
        ("status", task.status.to_string()),
        (
            "pipeline",
            task.pipeline.clone().unwrap_or_else(|| "-".into()),
        ),
        ("step", task.step.clone().unwrap_or_else(|| "-".into())),
        ("round", task.round.to_string()),
        ("repo", task.repo.clone()),
    ];
    if task.human_owned {
        rows.push(("owner", "you — status events for its pane are muted".into()));
    }
    for (label, value) in [
        ("worktree", &task.worktree),
        ("branch", &task.branch),
        ("base", &task.base),
        ("workspace", &task.workspace_id),
        ("pane", &pane),
    ] {
        if let Some(value) = value {
            rows.push((label, value.clone()));
        }
    }
    let width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    for (key, value) in rows {
        println!("{key:<width$}  {value}", width = width);
    }

    if !checks.is_empty() {
        println!("\nchecks");
        let now = db::now();
        for c in &checks {
            println!(
                "  {:<5} {:<8} {:<14} {:<10} {}",
                c.id,
                c.conclusion.as_str(),
                c.author,
                super::ago(now - c.created),
                super::truncate(c.body.as_deref().unwrap_or(""), 40)
            );
        }
    }
    Ok(())
}

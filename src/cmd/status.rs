use crate::db::{self, meta, schema, task};
use crate::supervisor::{self, Health};
use crate::{Error, paths};
use anyhow::Result;
use std::path::Path;

/// `shep status` — is anything driving this store, and what is in it?
///
/// A pure read, so it works with the supervisor down; that is the point of it.
pub fn run(db_path: &Path, json: bool) -> Result<()> {
    let conn = match db::open_existing(db_path) {
        Ok(conn) => Some(conn),
        Err(Error::NoStore(_)) => None,
        Err(e) => return Err(e.into()),
    };

    let Some(conn) = conn else {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "store": db_path.to_string_lossy(),
                    "exists": false,
                    "supervisor": "down",
                })
            );
        } else {
            println!(
                "store       {} (absent — nothing created yet)",
                db_path.display()
            );
            println!("supervisor  down");
        }
        return Ok(());
    };

    let version = schema::version(&conn)?;
    let paused = meta::is_paused(&conn)?;
    let health = supervisor::health(&conn)?;
    let counts = task::counts_by_status(&conn)?;
    let count_of = |s: task::Status| {
        counts
            .iter()
            .find(|(status, _)| *status == s)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    };

    if json {
        let hb = health.heartbeat();
        println!(
            "{}",
            serde_json::json!({
                "store": db_path.to_string_lossy(),
                "exists": true,
                "schema": version,
                "paused": paused,
                "supervisor": health.label(),
                "healthy": health.is_healthy(),
                "pid": hb.map(|h| h.pid),
                "last_beat": hb.map(|h| h.beat),
                "log": paths::log_path_for(db_path).to_string_lossy(),
                "tasks": task::Status::ALL
                    .iter()
                    .map(|s| (s.as_str().to_string(), serde_json::json!(count_of(*s))))
                    .collect::<serde_json::Map<String, serde_json::Value>>(),
            })
        );
        return Ok(());
    }

    println!("store       {} (schema v{version})", db_path.display());
    println!("log         {}", paths::log_path_for(db_path).display());
    println!("paused      {}", if paused { "yes" } else { "no" });
    println!("supervisor  {}", describe(&health));
    println!(
        "tasks       queued {}  running {}  parked {}  resting {}  finished {}  cancelled {}",
        count_of(task::Status::Queued),
        count_of(task::Status::Running),
        count_of(task::Status::Parked),
        count_of(task::Status::Resting),
        count_of(task::Status::Finished),
        count_of(task::Status::Cancelled),
    );
    Ok(())
}

fn describe(health: &Health) -> String {
    match health {
        Health::Down => "down".to_string(),
        Health::Running { heartbeat, age } => format!(
            "running  pid {}  last beat {}",
            heartbeat.pid,
            super::ago(*age)
        ),
        Health::Stalled { heartbeat, age } => format!(
            "STALLED  pid {} is alive but has not beaten for {}s",
            heartbeat.pid, age
        ),
        Health::Dead { heartbeat, age } => format!(
            "down     (pid {} died without cleaning up; last beat {})",
            heartbeat.pid,
            super::ago(*age)
        ),
    }
}

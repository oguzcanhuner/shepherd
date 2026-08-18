//! One module per subcommand. Each is a thin shell over `engine` and `db`: the
//! CLI holds no logic that the supervisor does not also run.

pub mod bind_pane;
pub mod cancel;
pub mod check;
pub mod context;
pub mod create;
pub mod forward;
pub mod get;
pub mod pause;
pub mod ps;
pub mod raw;
pub mod retry;
pub mod settle;
pub mod status;
pub mod supervise;
pub mod trace;
pub mod types;
pub mod validate;

/// The environment a step script is given (PLAN §7.1).
pub const TASK_ENV: &str = "SHEP_TASK_ID";
/// What Herdr injects into every managed pane (herdr-findings §2).
pub const PANE_ENV: &str = "HERDR_PANE_ID";

/// Which task this invocation is about.
///
/// `--task` wins, then `$SHEP_TASK_ID`, which every step script has. Last comes
/// `$HERDR_PANE_ID` through `pane_task`, which is all an agent in a pane has —
/// and the reason a bare `shep context` works at all (PLAN §6).
pub fn task_id(conn: &rusqlite::Connection, given: Option<String>) -> anyhow::Result<String> {
    if let Some(id) = given {
        return Ok(id);
    }
    if let Some(id) = std::env::var(TASK_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        return Ok(id);
    }
    let pane = std::env::var(PANE_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no task given: pass --task, or run this where ${TASK_ENV} or ${PANE_ENV} is set"
            )
        })?;
    crate::db::pane::task_for(conn, &pane)?.ok_or_else(|| {
        anyhow::anyhow!("pane {pane} is not bound to a task — has its step run `shep bind-pane`?")
    })
}

/// Which repo's policy governs this invocation.
///
/// Config is loaded per repo root, not globally (PLAN §4), so every command that
/// touches policy has to answer this the same way.
pub fn repo_root(given: Option<std::path::PathBuf>) -> anyhow::Result<std::path::PathBuf> {
    if let Some(path) = given {
        return std::fs::canonicalize(&path)
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()));
    }
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output();
    if let Some(out) = out.ok().filter(|o| o.status.success()) {
        let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !root.is_empty() {
            return Ok(std::path::PathBuf::from(root));
        }
    }
    Ok(std::env::current_dir()?)
}

/// A duration rendered the way you'd say it out loud. Used by every table.
pub fn ago(seconds: i64) -> String {
    let s = seconds.max(0);
    match s {
        0..=1 => "just now".to_string(),
        2..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86_399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

/// Cut a brief down to a table cell without lying about it.
pub fn truncate(text: &str, width: usize) -> String {
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= width {
        return one_line;
    }
    let keep: String = one_line.chars().take(width.saturating_sub(1)).collect();
    format!("{}…", keep.trim_end())
}

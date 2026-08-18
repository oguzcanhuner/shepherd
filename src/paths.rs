//! Where things live on disk.
//!
//! One store per machine: tasks carry a `repo` column, so a single database
//! spans every repo you work in. Config, by contrast, is per repo root.

use std::path::PathBuf;

pub const DB_ENV: &str = "SHEP_DB";
pub const BIN_ENV: &str = "SHEP_BIN";

/// `$XDG_STATE_HOME/shep`, else `~/.local/state/shep`, else `./.shep-state`.
pub fn state_dir() -> PathBuf {
    if let Some(xdg) = env_path("XDG_STATE_HOME") {
        return xdg.join("shep");
    }
    match env_path("HOME") {
        Some(home) => home.join(".local/state/shep"),
        None => PathBuf::from(".shep-state"),
    }
}

/// The store path: `$SHEP_DB` if set, else `<state_dir>/shep.db`.
///
/// `$SHEP_DB` is exported into every step script (PLAN §7.1) so that `shep`
/// subcommands a script invokes hit the same store the supervisor is driving.
pub fn db_path() -> PathBuf {
    env_path(DB_ENV).unwrap_or_else(|| state_dir().join("shep.db"))
}

/// The `shep` a step script should call.
///
/// A step script has to be able to reach back — `shep bind-pane`, `shep check
/// submit` — and it cannot assume the binary is on `$PATH`: in a Herdr plugin it
/// lives at `<plugin root>/target/release/shep`. So the running process names
/// itself, which is right whether that is an installed binary or one built in a
/// checkout. `$SHEP_BIN` overrides, for the same reason `hooks/lib.sh` honours it.
pub fn shep_bin() -> Option<PathBuf> {
    env_path(BIN_ENV).or_else(|| std::env::current_exe().ok())
}

/// Supervisor log file, kept beside the store so an alternate `$SHEP_DB` gets
/// its own log. Hook and step stdout is not a terminal, so logs go to a file
/// rather than to stdout (PLAN §3).
pub fn log_path() -> PathBuf {
    log_path_for(&db_path())
}

/// The log that belongs to a particular store, for when `--db` overrides.
pub fn log_path_for(db: &std::path::Path) -> PathBuf {
    match db.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join("supervisor.log"),
        _ => PathBuf::from("supervisor.log"),
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    match std::env::var_os(key) {
        Some(v) if !v.is_empty() => Some(PathBuf::from(v)),
        _ => None,
    }
}

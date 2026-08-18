// Shared by several test binaries; each compiles it separately, so not every
// helper is used in every one.
#![allow(dead_code)]

use shepherd::db::task::{NewTask, Task};
use std::path::{Path, PathBuf};

/// A store in a temp dir that cleans itself up with the test.
pub struct Store {
    /// Held for its Drop: the directory lives as long as the store does.
    pub _dir: tempfile::TempDir,
    pub path: PathBuf,
}

impl Store {
    pub fn new() -> Store {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("shep.db");
        // Create and migrate up front so every connection afterwards is a plain open.
        shepherd::db::open(&path).expect("open store");
        Store { _dir: dir, path }
    }

    pub fn conn(&self) -> rusqlite::Connection {
        shepherd::db::open(&self.path).expect("open store")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn task(&self, brief: &str) -> Task {
        let mut conn = self.conn();
        shepherd::engine::create_task(
            &mut conn,
            NewTask {
                brief: brief.to_string(),
                kind: "feature".to_string(),
                repo: "/tmp/repo".to_string(),
            },
        )
        .expect("create task")
    }
}

/// A pid that is certainly not running: spawn a process and reap it.
pub fn dead_pid() -> i32 {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn true");
    let pid = child.id() as i32;
    child.wait().expect("wait");
    pid
}

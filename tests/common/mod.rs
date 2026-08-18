// Shared by several test binaries; each compiles it separately, so not every
// helper is used in every one.
#![allow(dead_code)]

use shepherd::config::Policy;
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

/// A repo with a .shep directory, so config can be written and scripts made.
pub struct Repo {
    dir: tempfile::TempDir,
}

impl Repo {
    pub fn new() -> Repo {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join(".shep/scripts")).expect("mkdir");
        Repo { dir }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The filename is the registration (PLAN §4), so making a step means making
    /// an executable file.
    pub fn script(&self, name: &str) -> &Repo {
        let path = self.dir.path().join(format!(".shep/scripts/{name}.sh"));
        std::fs::write(
            &path,
            "#!/usr/bin/env bash\necho '{\"outcome\":\"pass\"}'\n",
        )
        .expect("write");
        make_executable(&path);
        self
    }

    /// A script that exists but cannot be run.
    pub fn unrunnable_script(&self, name: &str) -> &Repo {
        let path = self.dir.path().join(format!(".shep/scripts/{name}.sh"));
        std::fs::write(&path, "#!/usr/bin/env bash\n").expect("write");
        let mut perms = std::fs::metadata(&path).expect("meta").permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).expect("chmod");
        self
    }

    pub fn write(&self, config: &str) -> PathBuf {
        let path = self.dir.path().join(".shep/config.toml");
        std::fs::write(&path, config).expect("write config");
        path
    }

    pub fn load(&self, config: &str) -> shepherd::Result<Policy> {
        self.write(config);
        Policy::load(self.root())
    }

    /// The problems a config has, as one string, for asserting on wording.
    pub fn problems(&self, config: &str) -> String {
        match self.load(config) {
            Ok(_) => String::new(),
            Err(e) => e.to_string(),
        }
    }
}

pub fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).expect("meta").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod");
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

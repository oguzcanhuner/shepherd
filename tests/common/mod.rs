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
        self.task_in("/tmp/repo", "feature", brief)
    }

    /// A task governed by a repo that exists, for anything that will actually run.
    pub fn task_in(&self, repo: &str, kind: &str, brief: &str) -> Task {
        // The type seeds the plan, exactly as `shep create` does.
        let plan = shepherd::config::Policy::load(std::path::Path::new(repo))
            .ok()
            .and_then(|p| p.config.types.get(kind).map(|t| t.pipelines.clone()))
            .unwrap_or_default();
        let mut conn = self.conn();
        shepherd::engine::create_task(
            &mut conn,
            NewTask {
                brief: brief.to_string(),
                kind: kind.to_string(),
                repo: repo.to_string(),
                plan,
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

    /// The filename is the registration, so making a step means making
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

    /// A step script with a body of your own. `$SHEP_*` is in scope.
    pub fn script_with(&self, name: &str, body: &str) -> &Repo {
        let path = self.dir.path().join(format!(".shep/scripts/{name}.sh"));
        std::fs::write(&path, format!("#!/usr/bin/env bash\nset -u\n{body}\n")).expect("write");
        make_executable(&path);
        self
    }

    /// A step that records that it ran, in order, and passes.
    pub fn recording_script(&self, name: &str) -> &Repo {
        self.script_with(
            name,
            &format!(
                "echo '{name}' >> \"$SHEP_REPO/.shep/order\"\n                 echo \"$SHEP_PIPELINE/$SHEP_STEP round $SHEP_ROUND\" >> \"$SHEP_REPO/.shep/positions\"\n                 echo '{{\"outcome\":\"pass\"}}'"
            ),
        )
    }

    /// The steps that have run, in order.
    pub fn order(&self) -> Vec<String> {
        std::fs::read_to_string(self.root().join(".shep/order"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The positions those steps ran at, as `pipeline/step round N`.
    pub fn positions(&self) -> Vec<String> {
        std::fs::read_to_string(self.root().join(".shep/positions"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
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

impl Repo {
    /// A git repo with one commit, for anything that stamps a sha.
    ///
    /// Committer identity is passed per command rather than written to a config,
    /// so the test does not care what the machine's global git config says.
    pub fn git_init(&self) -> &Repo {
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.email=shep@test", "-c", "user.name=shep"])
                .args(args)
                .current_dir(self.root())
                .output()
                .expect("run git");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "the config this task is governed by"]);
        self
    }

    /// `git rev-parse HEAD`, which is what a check's sha has to equal.
    pub fn head(&self) -> String {
        let out = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(self.root())
            .output()
            .expect("run git");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

/// A repo whose policy is one deferred pipeline and then a synchronous one.
///
/// `launch.sh` stands in for `code.sh`: it binds a pane and returns `started`,
/// with no Herdr anywhere. That is the point — resolution is driven by rows in
/// `raw_event`, so testing it needs a store and a script, not a session.
pub fn deferred_repo(shep: &str) -> Repo {
    let repo = Repo::new();
    let launch = format!(
        r#"pane=${{SHEP_PANE:-wZ:$SHEP_TASK_ID}}
{shep} bind-pane "$pane" --workspace wZ --worktree "$SHEP_REPO" \
  --branch "shep/$SHEP_TASK_ID" --base main >/dev/null || exit 1
printf '{{"outcome":"started","pane":"%s"}}\n' "$pane""#
    );
    repo.script_with("launch", &launch);
    repo.recording_script("verify");
    repo.write(
        r#"
[pipeline.implement]
steps = [{ run = "launch", await = "agent_stopped" }]

[pipeline.after]
steps = ["verify"]

[type.watched]
description = "An agent in a pane, then a synchronous step of its own."
pipelines = ["implement", "after"]
"#,
    );
    repo
}

pub fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).expect("meta").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod");
}

/// A repo whose policy is one synchronous pipeline of one step.
///
/// `outcome.sh` is the step, and it says whatever `.shep/outcome` tells it to,
/// which is how a test decides what a step reports without writing a new script.
pub fn scripted_repo() -> Repo {
    let repo = Repo::new();
    let path = repo.root().join(".shep/scripts/outcome.sh");
    std::fs::write(
        &path,
        r#"#!/usr/bin/env bash
# Print whatever the test asked for. Everything before the last line is logs.
set -u
echo "running $SHEP_STEP for $SHEP_TASK_ID (round $SHEP_ROUND)"
env | grep '^SHEP_' | sort > "$SHEP_REPO/.shep/last-env"
pwd > "$SHEP_REPO/.shep/last-cwd"
cat "$SHEP_REPO/.shep/outcome"
exit "$(cat "$SHEP_REPO/.shep/exit" 2>/dev/null || echo 0)"
"#,
    )
    .expect("write outcome.sh");
    make_executable(&path);
    repo.write(
        r#"
[pipeline.check]
steps = ["outcome"]

[type.simple]
description = "One synchronous step, which says what the test told it to."
pipelines = ["check"]
"#,
    );
    repo.says(r#"{"outcome":"pass"}"#);
    repo
}

impl Repo {
    /// What `outcome.sh` will print as its last line.
    pub fn says(&self, verdict: &str) -> &Repo {
        std::fs::write(self.root().join(".shep/outcome"), format!("{verdict}\n"))
            .expect("write outcome");
        self
    }

    /// What `outcome.sh` will exit with.
    pub fn exits(&self, code: i32) -> &Repo {
        std::fs::write(self.root().join(".shep/exit"), format!("{code}\n")).expect("write exit");
        self
    }

    /// The SHEP_* environment the last step run actually saw.
    pub fn last_env(&self) -> std::collections::BTreeMap<String, String> {
        std::fs::read_to_string(self.root().join(".shep/last-env"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    pub fn last_cwd(&self) -> String {
        std::fs::read_to_string(self.root().join(".shep/last-cwd"))
            .unwrap_or_default()
            .trim()
            .to_string()
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

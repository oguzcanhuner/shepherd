//! The step script contract (PLAN §7.1).
//!
//! In: environment. Out: the last line of stdout is one JSON object. Everything
//! above it is logs, so a script is free to shell out to `pytest` or `claude -p`
//! and let it print.
//!
//! Nothing here can fail: a broken script, a missing script, a crash and a lie
//! all come back as `error`, because parking a task is the engine's answer to all
//! of them.

use crate::Outcome;
use crate::config::Policy;
use crate::db::task::Task;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How much of a script's output to keep for the log. A `pytest` run can be
/// enormous and the verdict is what matters; the tail is what says why.
const LOG_TAIL: usize = 4_000;

/// Where a task is: the position a report is about.
///
/// A synchronous step reports on the position it was spawned at; a deferred one
/// is resolved by an event that arrives long afterwards. Both have to name the
/// same three things, because that triple is what the guard in
/// [`finish_step`](super::finish_step) compares against the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepAt {
    pub pipeline: String,
    pub step: String,
    pub round: i64,
}

impl StepAt {
    pub fn new(pipeline: impl Into<String>, step: impl Into<String>, round: i64) -> StepAt {
        StepAt {
            pipeline: pipeline.into(),
            step: step.into(),
            round,
        }
    }

    /// Where a task row says it is, if it says anything.
    pub fn of(task: &Task) -> Option<StepAt> {
        match (task.pipeline.clone(), task.step.clone()) {
            (Some(pipeline), Some(step)) => Some(StepAt {
                pipeline,
                step,
                round: task.round,
            }),
            _ => None,
        }
    }
}

impl std::fmt::Display for StepAt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{} round {}", self.pipeline, self.step, self.round)
    }
}

/// One step invocation, resolved.
#[derive(Debug, Clone)]
pub struct StepSpec {
    pub task_id: String,
    pub kind: String,
    pub pipeline: String,
    pub step: String,
    pub round: i64,
    pub script: PathBuf,
    /// cwd for the script: the worktree once there is one, else the repo.
    pub cwd: PathBuf,
    pub repo: String,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub base: Option<String>,
    /// Present only if the task already has a bound pane.
    pub pane: Option<String>,
    /// So `shep` subcommands the script invokes find the right store.
    pub db: PathBuf,
}

impl StepSpec {
    /// Resolve what to run for a task sitting at a step.
    pub fn resolve(
        policy: &Policy,
        task: &Task,
        pipeline: &str,
        step: &str,
        round: i64,
        db: &Path,
        pane: Option<String>,
    ) -> crate::Result<StepSpec> {
        let script = policy.script_path(step).ok_or_else(|| {
            crate::Error::other(format!(
                "step {step:?} has no script — looked in {}",
                policy
                    .script_dirs
                    .iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        let cwd = task
            .worktree
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&task.repo));
        Ok(StepSpec {
            task_id: task.id.clone(),
            kind: task.kind.clone(),
            pipeline: pipeline.to_string(),
            step: step.to_string(),
            round,
            script,
            cwd,
            repo: task.repo.clone(),
            worktree: task.worktree.clone(),
            branch: task.branch.clone(),
            base: task.base.clone(),
            pane,
            db: db.to_path_buf(),
        })
    }

    /// The position this step is running at.
    pub fn at(&self) -> StepAt {
        StepAt::new(&self.pipeline, &self.step, self.round)
    }
}

/// What a step said.
#[derive(Debug, Clone)]
pub struct StepReport {
    pub outcome: Outcome,
    pub note: Option<String>,
    /// Only meaningful with `started`: the pane the work is happening in.
    pub pane: Option<String>,
    pub exit_code: Option<i32>,
    /// The tail of what the script printed, for the log.
    pub logs: String,
}

impl StepReport {
    /// A verdict reached without running anything: what resolving a deferred step
    /// produces (PLAN §7.2). There is no exit code and no output, because no
    /// process was involved — the answer came from a `check_run` row.
    pub fn verdict(outcome: Outcome, note: impl Into<String>) -> StepReport {
        StepReport {
            outcome,
            note: Some(note.into()),
            pane: None,
            exit_code: None,
            logs: String::new(),
        }
    }

    fn error(note: impl Into<String>, exit_code: Option<i32>, logs: String) -> StepReport {
        StepReport {
            outcome: Outcome::Error,
            note: Some(note.into()),
            pane: None,
            exit_code,
            logs,
        }
    }
}

/// The JSON a step is allowed to end with.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Verdict {
    outcome: Outcome,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    pane: Option<String>,
}

/// Run one step to completion.
///
/// Blocking on purpose: the supervisor gives each in-flight step a thread and
/// waits on the child, which at three or four concurrent tasks is cheaper and far
/// simpler than an async runtime (PLAN §3).
pub fn run(spec: &StepSpec) -> StepReport {
    let mut command = Command::new(&spec.script);
    command.current_dir(&spec.cwd);
    for (key, value) in environment(spec) {
        command.env(key, value);
    }

    let output = match command.output() {
        Ok(output) => output,
        Err(e) => {
            return StepReport::error(
                format!("could not run {}: {e}", spec.script.display()),
                None,
                String::new(),
            );
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let logs = combine(&stdout, &stderr);

    // Non-zero exit is an error regardless of what was printed: a script that
    // failed and still claimed to pass is exactly the case worth not believing.
    if !output.status.success() {
        return StepReport::error(
            format!("exited {}", describe_status(&output.status)),
            output.status.code(),
            logs,
        );
    }

    let Some(last) = stdout.lines().filter(|l| !l.trim().is_empty()).next_back() else {
        return StepReport::error(
            "printed nothing — the last line of stdout must be one JSON object",
            output.status.code(),
            logs,
        );
    };

    match serde_json::from_str::<Verdict>(last.trim()) {
        Ok(verdict) => StepReport {
            outcome: verdict.outcome,
            note: verdict.note,
            pane: verdict.pane,
            exit_code: output.status.code(),
            logs,
        },
        Err(e) => StepReport::error(
            format!(
                "last line of stdout is not a verdict ({e}): {}",
                tail(last, 200)
            ),
            output.status.code(),
            logs,
        ),
    }
}

/// The environment of PLAN §7.1, plus whatever Herdr injected into the supervisor.
pub fn environment(spec: &StepSpec) -> Vec<(String, String)> {
    let mut env = vec![
        ("SHEP_TASK_ID".to_string(), spec.task_id.clone()),
        ("SHEP_TYPE".to_string(), spec.kind.clone()),
        ("SHEP_PIPELINE".to_string(), spec.pipeline.clone()),
        ("SHEP_STEP".to_string(), spec.step.clone()),
        ("SHEP_ROUND".to_string(), spec.round.to_string()),
        ("SHEP_REPO".to_string(), spec.repo.clone()),
        ("SHEP_DB".to_string(), spec.db.display().to_string()),
    ];
    // An addition to §7.1: a step script has to be able to call back in, and
    // `shep` is not necessarily on $PATH — inside a Herdr plugin it is
    // `<plugin root>/target/release/shep`. Naming it beats every script
    // re-deriving it.
    if let Some(bin) = crate::paths::shep_bin() {
        env.push(("SHEP_BIN".to_string(), bin.display().to_string()));
    }
    // Absent rather than empty: a script testing `-n "$SHEP_WORKTREE"` should get
    // the truth.
    for (key, value) in [
        ("SHEP_WORKTREE", &spec.worktree),
        ("SHEP_BRANCH", &spec.branch),
        ("SHEP_BASE", &spec.base),
        ("SHEP_PANE", &spec.pane),
    ] {
        if let Some(value) = value {
            env.push((key.to_string(), value.clone()));
        }
    }
    env
}

fn combine(stdout: &str, stderr: &str) -> String {
    let mut logs = String::new();
    if !stdout.trim().is_empty() {
        logs.push_str(&tail(stdout, LOG_TAIL));
    }
    if !stderr.trim().is_empty() {
        if !logs.is_empty() {
            logs.push('\n');
        }
        logs.push_str("stderr: ");
        logs.push_str(&tail(stderr, LOG_TAIL));
    }
    logs
}

/// The end of a string, which is where the reason usually is.
fn tail(text: &str, keep: usize) -> String {
    let count = text.chars().count();
    if count <= keep {
        return text.to_string();
    }
    let skipped = count - keep;
    format!(
        "… [{skipped} characters omitted] …{}",
        text.chars().skip(skipped).collect::<String>()
    )
}

fn describe_status(status: &std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("with status {code}"),
        None => {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                if let Some(signal) = status.signal() {
                    return format!("on signal {signal}");
                }
            }
            "abnormally".to_string()
        }
    }
}

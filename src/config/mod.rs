//! Config: pipelines and types, loaded per repo root.
//!
//! `lint.sh` for a Rails app and for a Python library are not the same script,
//! and both should be versioned with the code they judge — so policy lives in
//! the repo being worked on, not next to the engine.

mod model;
mod validate;

pub use model::{
    BUILTIN_SIGNALS, Config, Pipeline, SIGNAL_AGENT_STOPPED, Signal, Step, StepDef, StepKind,
    TaskType, parse_duration,
};
pub use validate::{Problem, report, resolved_steps, validate};

use crate::{Error, Result};
use std::path::{Path, PathBuf};

/// Where policy lives inside a repo.
pub const CONFIG_RELATIVE: &str = ".shep/config.toml";
pub const SCRIPTS_RELATIVE: &str = ".shep/scripts";

/// A validated config, together with where it came from. Nothing outside this
/// module can build one without validation having run.
#[derive(Debug, Clone)]
pub struct Policy {
    pub config: Config,
    /// The repo root this governs.
    pub repo: PathBuf,
    /// The config file itself, for error messages.
    pub path: PathBuf,
    /// Where step scripts are looked for, in order, resolved once when the policy
    /// is built. Held rather than recomputed so that a `Policy` answers the same
    /// question the same way for its whole life, whatever the environment does
    /// afterwards.
    pub script_dirs: Vec<PathBuf>,
}

impl Policy {
    /// Load and validate `<repo>/.shep/config.toml`.
    pub fn load(repo: &Path) -> Result<Policy> {
        let (text, path) = read_config(repo)?;
        Policy::parse(&text, repo, &path)
    }

    /// Parse and validate config text. Separated from the read so tests and
    /// `shep validate` share exactly one path through the rules.
    pub fn parse(text: &str, repo: &Path, path: &Path) -> Result<Policy> {
        Policy::parse_in(text, repo, path, script_search_path(repo))
    }

    /// Parse and validate, looking for step scripts in the given directories.
    /// The search path is a parameter so that testing the fallback location does
    /// not mean reaching for a process-wide `HOME`.
    pub fn parse_in(
        text: &str,
        repo: &Path,
        path: &Path,
        script_dirs: Vec<PathBuf>,
    ) -> Result<Policy> {
        let policy = Policy::parse_only_in(text, repo, path, script_dirs)?;
        let problems = policy.problems();
        if !problems.is_empty() {
            return Err(Error::other(validate::report(&policy, &problems)));
        }
        Ok(policy)
    }

    /// Parse without validating, for `shep validate` — which wants to report
    /// every problem rather than fail at the first.
    pub fn parse_only(text: &str, repo: &Path, path: &Path) -> Result<Policy> {
        Policy::parse_only_in(text, repo, path, script_search_path(repo))
    }

    fn parse_only_in(
        text: &str,
        repo: &Path,
        path: &Path,
        script_dirs: Vec<PathBuf>,
    ) -> Result<Policy> {
        let config: Config = toml::from_str(text)
            // toml's message carries the line, the column and a source excerpt.
            // Truncating it would throw away the sentence naming the bad key.
            .map_err(|e| Error::other(format!("{}: {e}", path.display())))?;
        Ok(Policy {
            config,
            repo: repo.to_path_buf(),
            path: path.to_path_buf(),
            script_dirs,
        })
    }

    /// Read the config file without validating it.
    pub fn read_only(repo: &Path) -> Result<Policy> {
        let (text, path) = read_config(repo)?;
        Policy::parse_only(&text, repo, &path)
    }

    pub fn problems(&self) -> Vec<Problem> {
        validate(self)
    }

    /// The type an agent asked for, or the menu of what it could have asked for.
    pub fn task_type(&self, name: &str) -> Result<&TaskType> {
        self.config
            .types
            .get(name)
            .ok_or_else(|| Error::other(format!("unknown type {name:?}. {}", self.type_menu())))
    }

    /// What `shep types` prints, and what an invalid `--type` error lists.
    pub fn type_menu(&self) -> String {
        if self.config.types.is_empty() {
            return format!("{} defines no types", self.path.display());
        }
        let mut menu = String::from("Available:");
        for (name, t) in &self.config.types {
            menu.push_str(&format!("\n  {name:<12} {}", t.description));
        }
        menu
    }

    pub fn pipeline(&self, name: &str) -> Result<&Pipeline> {
        self.config
            .pipeline
            .get(name)
            .ok_or_else(|| Error::other(format!("unknown pipeline {name:?}")))
    }

    /// The signal a step defers on, if it is a deferred step.
    pub fn step_await(&self, pipeline: &str, step: &str) -> Option<&str> {
        self.config.pipeline.get(pipeline)?.step(step)?.await_on()
    }

    /// A step's `on_missing` fallback.
    pub fn step_on_missing(&self, pipeline: &str, step: &str) -> Option<crate::Outcome> {
        self.config.pipeline.get(pipeline)?.step(step)?.on_missing()
    }

    /// A step's timeout in seconds, if it declares a parseable one.
    pub fn step_timeout_secs(&self, pipeline: &str, step: &str) -> Option<i64> {
        self.config.pipeline.get(pipeline)?.step(step)?.timeout_secs()
    }

    /// A step's `on_timeout` verdict.
    pub fn step_on_timeout(&self, pipeline: &str, step: &str) -> Option<crate::Outcome> {
        self.config.pipeline.get(pipeline)?.step(step)?.on_timeout()
    }

    /// Is this signal name one shepherd knows — a built-in or a declared one?
    pub fn signal_known(&self, name: &str) -> bool {
        crate::config::BUILTIN_SIGNALS.contains(&name) || self.config.signal.contains_key(name)
    }

    /// Every signal name that a step may `await`, built-ins first.
    pub fn known_signals(&self) -> Vec<String> {
        let mut names: Vec<String> = crate::config::BUILTIN_SIGNALS
            .iter()
            .map(|s| s.to_string())
            .collect();
        names.extend(self.config.signal.keys().cloned());
        names
    }

    /// What a step name means: a script, or another pipeline. A name that is
    /// neither is a validation error, so callers of a validated policy can treat
    /// `None` as impossible.
    ///
    /// A script wins over a pipeline of the same name. That order is what lets
    /// `[pipeline.integrate] steps = ["integrate"]` mean "run integrate.sh"
    /// rather than "recurse into yourself", which is how anyone reads it.
    /// Validation still rejects the genuinely ambiguous case: a step naming a
    /// *different* pipeline that a script also shadows.
    pub fn step_kind(&self, step: &str) -> Option<StepKind> {
        if let Some(path) = self.script_path(step) {
            return Some(StepKind::Script(path));
        }
        if self.config.pipeline.contains_key(step) {
            return Some(StepKind::Pipeline(step.to_string()));
        }
        None
    }

    /// Does this step name run a nested pipeline? Only if no script shadows it,
    /// which is what keeps composition and self-naming apart.
    pub fn nested_pipeline(&self, step: &str) -> Option<&str> {
        match self.step_kind(step) {
            Some(StepKind::Pipeline(_)) => self
                .config
                .pipeline
                .get_key_value(step)
                .map(|(k, _)| k.as_str()),
            _ => None,
        }
    }

    /// The filename is the registration: `steps = ["lint"]` resolves to
    /// `.shep/scripts/lint.sh`, with `~/.config/shep/scripts/` as the fallback
    /// for project-agnostic scripts.
    pub fn script_path(&self, step: &str) -> Option<PathBuf> {
        self.script_dirs
            .iter()
            .map(|dir| dir.join(format!("{step}.sh")))
            .find(|p| is_executable_file(p))
    }
}

fn read_config(repo: &Path) -> Result<(String, PathBuf)> {
    let path = repo.join(CONFIG_RELATIVE);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::other(format!(
                "no {CONFIG_RELATIVE} in {} — shepherd needs its policy in the repo it works on",
                repo.display()
            ))
        } else {
            Error::other(format!("reading {}: {e}", path.display()))
        }
    })?;
    Ok((text, path))
}

/// Where a step script may live, in the order searched.
pub fn script_search_path(repo: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![repo.join(SCRIPTS_RELATIVE)];
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        dirs.push(PathBuf::from(home).join(".config/shep/scripts"));
    }
    dirs
}

/// A step script must be a file you can actually run. A non-executable script is
/// the sort of thing that would otherwise fail at 2am.
pub fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

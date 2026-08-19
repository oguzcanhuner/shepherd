//! The config schema. `deny_unknown_fields` throughout: a typo must
//! be an error, not silence.

use crate::outcome::Outcome;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The built-in signal an agent stopping in a task's pane emits.
pub const SIGNAL_AGENT_STOPPED: &str = "agent_stopped";
/// Signals shepherd provides without declaration, because it emits them itself.
/// A person is *not* one of them: humans are not in the state machine — a task
/// rests between pipelines and a person applies the next with `shep run`.
pub const BUILTIN_SIGNALS: [&str; 1] = [SIGNAL_AGENT_STOPPED];

/// `<repo>/.shep/config.toml`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Pipelines by name. A pipeline is a state machine over steps; it owns its
    /// loop and its round cap, and it returns an outcome, which is why a
    /// pipeline can be used as a step.
    #[serde(default)]
    pub pipeline: BTreeMap<String, Pipeline>,

    /// Types by name. A type is a composition of pipelines with no loops, so
    /// termination is obvious. This is what an agent picks.
    #[serde(default, rename = "type")]
    pub types: BTreeMap<String, TaskType>,

    /// Custom signals a step may `await`. The built-in signals
    /// ([`BUILTIN_SIGNALS`]) need no declaration; anything else a step waits on
    /// must be declared here, so a typo in `await` is an error rather than a
    /// step that waits forever.
    #[serde(default)]
    pub signal: BTreeMap<String, Signal>,
}

/// A declared signal: a name an external emitter can resolve a deferred step
/// with, via `shep signal <task> --name <name>`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Signal {
    /// What this signal means and who emits it — the discoverability payoff of
    /// declaring it.
    #[serde(default)]
    pub description: String,
}

/// One step in a pipeline. Written either as a bare name (`"lint"`, a
/// synchronous script) or as a table that also says how the step defers:
/// `{ run = "code", await = "agent_stopped", on_missing = "pass" }`.
///
/// The name is the identity: a task records its position by step name, `on_fail`
/// targets a step by name, and the planner compares by name. The table form only
/// adds *how the step completes* alongside that identity.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Step {
    /// `"lint"` — a synchronous script, no wait.
    Name(String),
    /// `{ run = "code", await = "agent_stopped", ... }`.
    Spec(StepDef),
}

/// The table form of a [`Step`].
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StepDef {
    /// The step's name — its identity.
    pub run: String,
    /// The signal that resolves this step when it returns `started`. A built-in
    /// (`agent_stopped`, `human`) or a declared `[signal.*]`. `await` is a Rust
    /// keyword, hence the rename.
    #[serde(rename = "await", default, skip_serializing_if = "Option::is_none")]
    pub await_on: Option<String>,
    /// What an agent stopping means when it left no check. Only meaningful with
    /// `await`. Unset means `error`: nothing says whether the work happened.
    /// `pass` is for steps whose work a later pipeline judges. A check, when
    /// there is one, always wins over this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_missing: Option<Outcome>,
    /// How long the wait may last, e.g. `"30m"`, `"2h"`, `"90s"`, `"1d"`. Only
    /// meaningful with `await`. When it elapses, the step resolves with
    /// `on_timeout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    /// The verdict when `timeout` fires. Defaults to `error` (park): a wait that
    /// ran out of time is not a silent pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_timeout: Option<Outcome>,
}

impl Step {
    /// The step's name, whichever form it was written in.
    pub fn name(&self) -> &str {
        match self {
            Step::Name(name) => name,
            Step::Spec(spec) => &spec.run,
        }
    }

    /// The signal this step defers on, if it is a deferred step.
    pub fn await_on(&self) -> Option<&str> {
        match self {
            Step::Name(_) => None,
            Step::Spec(spec) => spec.await_on.as_deref(),
        }
    }

    /// The step's `on_missing` fallback, if any.
    pub fn on_missing(&self) -> Option<Outcome> {
        match self {
            Step::Name(_) => None,
            Step::Spec(spec) => spec.on_missing,
        }
    }

    /// The raw timeout string, if any.
    pub fn timeout(&self) -> Option<&str> {
        match self {
            Step::Name(_) => None,
            Step::Spec(spec) => spec.timeout.as_deref(),
        }
    }

    /// The timeout as seconds, if set and parseable.
    pub fn timeout_secs(&self) -> Option<i64> {
        self.timeout().and_then(parse_duration)
    }

    /// The verdict a fired timeout produces.
    pub fn on_timeout(&self) -> Option<Outcome> {
        match self {
            Step::Name(_) => None,
            Step::Spec(spec) => spec.on_timeout,
        }
    }
}

/// Parse a duration like `"30m"`, `"2h"`, `"90s"`, `"1d"` into seconds. A bare
/// number is seconds. Returns `None` for anything it cannot read, which
/// validation reports as a problem.
pub fn parse_duration(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (digits, unit): (String, String) = s.chars().partition(|c| c.is_ascii_digit());
    if digits.is_empty() {
        return None;
    }
    let n: i64 = digits.parse().ok()?;
    let mult = match unit.trim() {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        _ => return None,
    };
    Some(n * mult)
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pipeline {
    /// Steps in order. Each names either `.shep/scripts/<step>.sh` or another
    /// pipeline, and may carry how it defers (see [`Step`]).
    pub steps: Vec<Step>,

    /// Where to go when a step rejects. A step inside *this* pipeline: round is
    /// scoped here, so a cross-pipeline target would be meaningless.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_fail: Option<String>,

    /// How many times round this pipeline's loop before giving up. Required
    /// wherever `on_fail` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,

    /// This pipeline's own outcome once `max_rounds` is spent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_exhausted: Option<Outcome>,
}

impl Pipeline {
    /// A pipeline with `on_fail` can go round again, which is what makes some
    /// things unsafe to nest inside it.
    pub fn loops(&self) -> bool {
        self.on_fail.is_some()
    }

    /// The step of this pipeline with the given name, looking in the forward
    /// sequence and at the `on_fail` repair step. `None` for a name that is not a
    /// step here.
    pub fn step(&self, name: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.name() == name)
    }

    /// Every step name in this pipeline, forward sequence then any `on_fail`
    /// repair step, in order.
    pub fn step_names(&self) -> impl Iterator<Item = &str> {
        self.steps
            .iter()
            .map(Step::name)
            .chain(self.on_fail.as_deref())
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskType {
    /// Why you would choose this type. It exists so an agent can choose, and the
    /// agent only ever chooses a type — so it is required.
    pub description: String,
    /// The pipelines to run, in order.
    pub pipelines: Vec<String>,
}

/// What a step name turned out to mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepKind {
    /// An executable script.
    Script(std::path::PathBuf),
    /// Another pipeline, used as a step for its outcome.
    Pipeline(String),
}

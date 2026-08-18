//! The config schema. `deny_unknown_fields` throughout: a typo must
//! be an error, not silence.

use crate::outcome::Outcome;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pipeline {
    /// Steps in order. Each names either `.shep/scripts/<step>.sh` or another
    /// pipeline.
    pub steps: Vec<String>,

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

    /// What resolves a deferred step: nothing (synchronous), an agent stopping,
    /// or a human. `await` is a Rust keyword, hence the rename.
    #[serde(default, rename = "await", skip_serializing_if = "Option::is_none")]
    pub await_on: Option<Await>,
}

impl Pipeline {
    /// A pipeline with `on_fail` can go round again, which is what makes some
    /// things unsafe to nest inside it.
    pub fn loops(&self) -> bool {
        self.on_fail.is_some()
    }
}

/// `await` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Await {
    /// The pane's agent going `done`, or the pane exiting. The outcome then comes
    /// from the latest matching check_run.
    AgentStopped,
    /// Nothing resolves it but `shep approve` / `shep reject`.
    Human,
}

impl Await {
    pub fn as_str(self) -> &'static str {
        match self {
            Await::AgentStopped => "agent_stopped",
            Await::Human => "human",
        }
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

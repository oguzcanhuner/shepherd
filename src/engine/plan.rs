//! Where a task goes next.
//!
//! Pure: config plus a task row in, one decision out. The engine decides nothing
//! a human didn't write down (PLAN §1), so this file has no I/O, no clock and no
//! opinions — every branch it takes comes from the config it was handed.

use crate::config::{Policy, StepKind, TaskType};
use crate::db::task::Task;

/// What to do with a task, given where it has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Run this step. `pipeline` is where round is scoped, which for a nested
    /// pipeline is the nested one.
    Run {
        pipeline: String,
        step: String,
        round: i64,
    },
    /// Every pipeline of the type is done.
    Finish,
    /// Nothing can proceed. Parking is inert: the task sits there until
    /// `shep retry` (PLAN §1).
    Park { reason: String },
}

impl Plan {
    fn park(reason: impl Into<String>) -> Plan {
        Plan::Park {
            reason: reason.into(),
        }
    }
}

/// The first thing to do with a freshly created task.
pub fn start(policy: &Policy, task: &Task) -> Plan {
    let Some(planner) = Planner::new(policy, task) else {
        return Plan::park(format!(
            "type {:?} is not in {} any more",
            task.kind,
            policy.path.display()
        ));
    };
    match planner.task_type.pipelines.first() {
        Some(first) => planner.enter(first),
        None => Plan::park(format!("type {:?} has no pipelines", task.kind)),
    }
}

/// Where a task goes when its current step passes.
///
/// Walking off the end of a pipeline is that pipeline passing, which is why a
/// pipeline can be used as a step: it returns an outcome like any other.
pub fn after_pass(policy: &Policy, task: &Task) -> Plan {
    let (Some(pipeline), Some(step)) = (task.pipeline.as_deref(), task.step.as_deref()) else {
        // Nothing to advance from — treat it as a start rather than inventing a
        // position.
        return start(policy, task);
    };
    let Some(planner) = Planner::new(policy, task) else {
        return Plan::park(format!("type {:?} is not in config any more", task.kind));
    };

    let Ok(current) = policy.pipeline(pipeline) else {
        return Plan::park(format!(
            "pipeline {pipeline:?} is not in config any more, so this task is stranded"
        ));
    };
    let Some(index) = current.steps.iter().position(|s| s == step) else {
        // Config was edited under a running task. Say so, rather than silently
        // restarting the pipeline from the top.
        return Plan::park(format!(
            "step {step:?} is no longer a step of pipeline {pipeline:?}"
        ));
    };

    match current.steps.get(index + 1) {
        Some(next) => planner.step_plan(pipeline, next),
        // The pipeline is out of steps, so it passed.
        None => planner.leave(pipeline),
    }
}

/// Everything the decision needs, so the recursion stays readable.
struct Planner<'a> {
    policy: &'a Policy,
    task_type: &'a TaskType,
    /// Where the task is now, which is what says whether a round carries over.
    current_pipeline: Option<&'a str>,
    current_round: i64,
}

impl<'a> Planner<'a> {
    fn new(policy: &'a Policy, task: &'a Task) -> Option<Planner<'a>> {
        Some(Planner {
            policy,
            task_type: policy.config.types.get(&task.kind)?,
            current_pipeline: task.pipeline.as_deref(),
            current_round: task.round,
        })
    }

    /// Begin a pipeline at its first step, descending if that step is itself a
    /// pipeline.
    fn enter(&self, pipeline: &str) -> Plan {
        let Ok(entered) = self.policy.pipeline(pipeline) else {
            return Plan::park(format!("no pipeline {pipeline:?} in config"));
        };
        match entered.steps.first() {
            Some(first) => self.step_plan(pipeline, first),
            None => Plan::park(format!("pipeline {pipeline:?} has no steps")),
        }
    }

    /// A step is either a script to run here, or a pipeline to descend into.
    fn step_plan(&self, pipeline: &str, step: &str) -> Plan {
        match self.policy.step_kind(step) {
            Some(StepKind::Script(_)) => Plan::Run {
                pipeline: pipeline.to_string(),
                step: step.to_string(),
                // Round is scoped to the innermost pipeline: it carries across
                // steps of the same pipeline and starts again in a new one.
                round: if self.current_pipeline == Some(pipeline) {
                    self.current_round
                } else {
                    0
                },
            },
            Some(StepKind::Pipeline(inner)) => self.enter(&inner),
            None => Plan::park(format!(
                "step {step:?} of pipeline {pipeline:?} resolves to nothing runnable"
            )),
        }
    }

    /// A pipeline finished with `pass`; continue after it.
    fn leave(&self, finished: &str) -> Plan {
        if let Some(index) = self.task_type.pipelines.iter().position(|p| p == finished) {
            return match self.task_type.pipelines.get(index + 1) {
                Some(next) => self.enter(next),
                None => Plan::Finish,
            };
        }

        // Not a pipeline of the type, so it was nested: continue in its parent,
        // after the step that named it. Nesting is capped at 2, so there is at
        // most one level to climb.
        let Some(parent) = self.parent_of(finished) else {
            return Plan::park(format!(
                "pipeline {finished:?} is not part of this type and nothing composes it"
            ));
        };
        let Ok(outer) = self.policy.pipeline(&parent) else {
            return Plan::park(format!("pipeline {parent:?} is not in config any more"));
        };
        match outer.steps.iter().position(|s| s == finished) {
            Some(index) => match outer.steps.get(index + 1) {
                Some(next) => self.step_plan(&parent, next),
                None => self.leave(&parent),
            },
            None => Plan::park(format!(
                "pipeline {finished:?} is no longer a step of {parent:?}"
            )),
        }
    }

    /// Which pipeline of this type composes `nested` as a step. Names are unique
    /// within a type (validated), so there is at most one answer.
    fn parent_of(&self, nested: &str) -> Option<String> {
        self.task_type
            .pipelines
            .iter()
            .find(|outer| {
                self.policy
                    .config
                    .pipeline
                    .get(outer.as_str())
                    .is_some_and(|p| p.steps.iter().any(|s| s == nested))
            })
            .cloned()
    }
}

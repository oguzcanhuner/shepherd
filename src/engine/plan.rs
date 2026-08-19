//! Where a task goes next.
//!
//! Pure: config plus a task row in, one decision out. The engine decides nothing
//! a human didn't write down, so this file has no I/O, no clock and no
//! opinions — every branch it takes comes from the config it was handed.

use crate::Outcome;
use crate::config::{Pipeline, Policy, StepKind};
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
    /// The task's plan is spent: it comes to rest until something applies more.
    Rest,
    /// Nothing can proceed. Parking is inert: the task sits there until
    /// `shep retry`.
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
    match planner.plan.first() {
        Some(first) => planner.enter(first),
        None => Plan::Rest,
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
    let Some(index) = current.steps.iter().position(|s| s.name() == step) else {
        // The repair step is a step of this pipeline without being in its
        // sequence, so passing it means the round it was reached in can now be
        // tried properly: back to the top, with the round the rejection set.
        if current.on_fail.as_deref() == Some(step) {
            return planner.restart(pipeline);
        }
        // Otherwise config was edited under a running task. Say so, rather than
        // silently restarting the pipeline from the top.
        return Plan::park(format!(
            "step {step:?} is no longer a step of pipeline {pipeline:?}"
        ));
    };

    match current.steps.get(index + 1) {
        Some(next) => planner.step_plan(pipeline, next.name()),
        // The pipeline is out of steps, so it passed.
        None => planner.leave(pipeline),
    }
}

/// Where a task goes when its current step rejects.
///
/// The pipeline owns its loop and its cap, so this is entirely a
/// question about the pipeline the task is in: it has somewhere for a rejection to
/// go, or it does not, and it has rounds left, or it is spent.
pub fn after_fail(policy: &Policy, task: &Task) -> Plan {
    let (Some(pipeline), Some(step)) = (task.pipeline.as_deref(), task.step.as_deref()) else {
        return Plan::park("a rejection arrived for a task that is nowhere");
    };
    let Some(planner) = Planner::new(policy, task) else {
        return Plan::park(format!("type {:?} is not in config any more", task.kind));
    };
    let Ok(current) = policy.pipeline(pipeline) else {
        return Plan::park(format!(
            "pipeline {pipeline:?} is not in config any more, so this task is stranded"
        ));
    };

    let (Some(target), Some(cap)) = (current.on_fail.as_deref(), current.max_rounds) else {
        // Validation rejects a cap without a target and a target without a cap, so
        // this is a pipeline that never meant to loop.
        return Plan::park(format!(
            "step {step:?} rejected and pipeline {pipeline:?} has no on_fail"
        ));
    };

    let next = task.round + 1;
    if next >= i64::from(cap) {
        return planner.exhausted(pipeline, current, step);
    }
    // A rejection of the repair step itself goes back to the repair step: on_fail
    // is where *any* rejection in this pipeline goes, and the cap is what makes
    // that terminate rather than a special case for it here.
    planner.step_at(pipeline, target, next)
}

/// Everything the decision needs, so the recursion stays readable.
struct Planner<'a> {
    policy: &'a Policy,
    /// The task's plan: the top-level pipelines it runs, in order. Read from the
    /// row, not the type — so a pipeline applied by hand has somewhere to return
    /// to, and a plan outlives edits to the type that seeded it.
    plan: &'a [String],
    /// Where the task is now, which is what says whether a round carries over.
    current_pipeline: Option<&'a str>,
    current_round: i64,
}

impl<'a> Planner<'a> {
    fn new(_policy: &'a Policy, task: &'a Task) -> Option<Planner<'a>> {
        Some(Planner {
            policy: _policy,
            plan: &task.plan,
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
            Some(first) => self.step_plan(pipeline, first.name()),
            None => Plan::park(format!("pipeline {pipeline:?} has no steps")),
        }
    }

    /// A step is either a script to run here, or a pipeline to descend into.
    fn step_plan(&self, pipeline: &str, step: &str) -> Plan {
        // Round is scoped to the innermost pipeline: it carries across steps of
        // the same pipeline and starts again in a new one.
        let round = if self.current_pipeline == Some(pipeline) {
            self.current_round
        } else {
            0
        };
        self.step_at(pipeline, step, round)
    }

    /// A step, at a round this decision has chosen: what a rejection does.
    fn step_at(&self, pipeline: &str, step: &str, round: i64) -> Plan {
        match self.policy.step_kind(step) {
            Some(StepKind::Script(_)) => Plan::Run {
                pipeline: pipeline.to_string(),
                step: step.to_string(),
                round,
            },
            Some(StepKind::Pipeline(inner)) => self.enter(&inner),
            None => Plan::park(format!(
                "step {step:?} of pipeline {pipeline:?} resolves to nothing runnable"
            )),
        }
    }

    /// Round again: the first step of this pipeline, at the round the rejection
    /// moved it to.
    fn restart(&self, pipeline: &str) -> Plan {
        let Ok(current) = self.policy.pipeline(pipeline) else {
            return Plan::park(format!("pipeline {pipeline:?} is not in config any more"));
        };
        match current.steps.first() {
            Some(first) => self.step_at(pipeline, first.name(), self.current_round),
            None => Plan::park(format!("pipeline {pipeline:?} has no steps")),
        }
    }

    /// The cap is spent. `on_exhausted` is this pipeline's own outcome, so it is
    /// handled exactly as an outcome from a step would be — which is what makes a
    /// pipeline usable as a step.
    fn exhausted(&self, pipeline: &str, current: &Pipeline, step: &str) -> Plan {
        let rounds = current.max_rounds.unwrap_or_default();
        let spent = format!("pipeline {pipeline:?} rejected {step:?} in all {rounds} rounds");
        // Absent means `reject`: the pipeline ran out of chances, and calling that
        // a pass would wave the very thing it was checking through.
        match current.on_exhausted.unwrap_or(Outcome::Reject) {
            Outcome::Pass => self.leave(pipeline),
            Outcome::Reject => self.rejected(pipeline, &spent),
            Outcome::Error => Plan::park(format!("{spent}, and on_exhausted says error")),
            // Validation rejects this; a pipeline that is finished cannot promise.
            Outcome::Started => Plan::park(format!(
                "{spent}, and on_exhausted says started, which a pipeline cannot mean"
            )),
        }
    }

    /// A pipeline whose own outcome is `reject`.
    ///
    /// At the top of a type that is the end of the road: a type is a composition
    /// with no loops, so there is nothing to send it back to and the task
    /// parks with what it was. Nested, the rejection would be its parent step's —
    /// but validation forbids nesting inside a looping pipeline, so the parent has
    /// no on_fail either, and parking is the honest answer there too.
    fn rejected(&self, pipeline: &str, why: &str) -> Plan {
        if self.plan.iter().any(|p| p == pipeline) {
            return Plan::park(format!("{why}, so {pipeline:?} rejected this task"));
        }
        match self.parent_of(pipeline) {
            Some(parent) => Plan::park(format!(
                "{why}, so it rejected — and {parent:?} has nowhere to send a rejection"
            )),
            None => Plan::park(format!("{why}, and nothing composes {pipeline:?}")),
        }
    }

    /// A pipeline finished with `pass`; continue after it.
    fn leave(&self, finished: &str) -> Plan {
        if let Some(index) = self.plan.iter().position(|p| p == finished) {
            return match self.plan.get(index + 1) {
                Some(next) => self.enter(next),
                // The plan is spent: the task comes to rest.
                None => Plan::Rest,
            };
        }

        // Not a top-level pipeline of the plan, so it was nested: continue in its
        // parent, after the step that named it. Nesting is capped at 2, so there
        // is at most one level to climb.
        let Some(parent) = self.parent_of(finished) else {
            return Plan::park(format!(
                "pipeline {finished:?} is not part of this plan and nothing composes it"
            ));
        };
        let Ok(outer) = self.policy.pipeline(&parent) else {
            return Plan::park(format!("pipeline {parent:?} is not in config any more"));
        };
        match outer.steps.iter().position(|s| s.name() == finished) {
            Some(index) => match outer.steps.get(index + 1) {
                Some(next) => self.step_plan(&parent, next.name()),
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
        self.plan
            .iter()
            .find(|outer| {
                self.policy
                    .config
                    .pipeline
                    .get(outer.as_str())
                    .is_some_and(|p| p.steps.iter().any(|s| s.name() == nested))
            })
            .cloned()
    }
}

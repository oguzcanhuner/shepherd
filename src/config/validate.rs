//! The config validation rules, plus three the data model forces.
//!
//! Every rule reports rather than returns, so one pass tells you everything
//! that is wrong with a config instead of the first thing.

use super::{Await, Policy, StepKind};
use crate::outcome::Outcome;
use std::collections::{BTreeMap, BTreeSet};

/// One thing wrong, addressed to where it is wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// `pipeline.review`, `type.feature`, or `config`.
    pub at: String,
    pub message: String,
    /// What to do about it, when that is not obvious from the message.
    pub hint: Option<String>,
}

impl Problem {
    fn new(at: impl Into<String>, message: impl Into<String>) -> Problem {
        Problem {
            at: at.into(),
            message: message.into(),
            hint: None,
        }
    }

    fn hint(mut self, hint: impl Into<String>) -> Problem {
        self.hint = Some(hint.into());
        self
    }
}

/// Every problem with this config, in a stable order.
pub fn validate(policy: &Policy) -> Vec<Problem> {
    let mut problems = Vec::new();
    let config = &policy.config;

    if config.types.is_empty() {
        problems.push(
            Problem::new("config", "no types defined, so nothing can be created").hint(
                "a type is what an agent picks: [type.feature] with a description and pipelines",
            ),
        );
    }

    for (name, pipeline) in &config.pipeline {
        let at = format!("pipeline.{name}");

        if pipeline.steps.is_empty() {
            problems.push(Problem::new(&at, "no steps, so this pipeline does nothing"));
        }

        // `task.step` records a step by name, so a name appearing twice
        // in one pipeline leaves the engine unable to say where it is.
        for duplicate in duplicates(&pipeline.steps) {
            problems.push(
                Problem::new(&at, format!("step {duplicate:?} appears more than once"))
                    .hint("a task records its position by step name, so names must be unique here"),
            );
        }

        // Rule: every step resolves to an executable file, or to another pipeline.
        for step in &pipeline.steps {
            problems.extend(unresolved(policy, &at, step, "step"));

            // A script wins over a same-named pipeline, so a step naming another
            // pipeline that a script also shadows would run something other than
            // what it reads as. Naming the *enclosing* pipeline is exempt: that
            // can only ever have meant the script.
            if step != name
                && config.pipeline.contains_key(step)
                && let Some(path) = policy.script_path(step)
            {
                problems.push(
                    Problem::new(
                        &at,
                        format!(
                            "step {step:?} names a pipeline, but the script {} shadows it",
                            path.display()
                        ),
                    )
                    .hint("rename one of them: a script of the same name always wins"),
                );
            }
        }

        // Rule: on_fail names a step of *this* pipeline's machine — round is
        // scoped here, so it cannot reach into another pipeline. Naming it here
        // is what makes it one of this pipeline's steps, so it need not also
        // appear in `steps`: a repair step that ran in the forward sequence would
        // run when nothing was wrong.
        if let Some(on_fail) = &pipeline.on_fail {
            problems.extend(unresolved(policy, &at, on_fail, "on_fail"));

            // Rule: on_fail without max_rounds is an unbounded loop.
            if pipeline.max_rounds.is_none() {
                problems.push(
                    Problem::new(
                        &at,
                        "on_fail is set but max_rounds is not: an unbounded loop",
                    )
                    .hint("give it a cap, e.g. max_rounds = 3"),
                );
            }
        } else {
            // A cap or an exhaustion outcome with nothing to loop is dead config,
            // and dead config is how you end up believing something is enforced.
            if pipeline.max_rounds.is_some() {
                problems.push(
                    Problem::new(
                        &at,
                        "max_rounds is set but on_fail is not, so nothing loops",
                    )
                    .hint("drop max_rounds, or say what a rejection should go back to"),
                );
            }
            if pipeline.on_exhausted.is_some() {
                problems.push(Problem::new(
                    &at,
                    "on_exhausted is set but on_fail is not, so the cap can never be spent",
                ));
            }
        }

        if let Some(0) = pipeline.max_rounds {
            problems.push(Problem::new(
                &at,
                "max_rounds = 0 would run the pipeline no times at all",
            ));
        }

        // A pipeline returns an outcome; `started` is a promise a step makes, and
        // a pipeline that is finished cannot be making promises.
        if pipeline.on_exhausted == Some(Outcome::Started) {
            problems.push(
                Problem::new(
                    &at,
                    "on_exhausted = \"started\" is not an outcome a pipeline can have",
                )
                .hint("use pass, reject or error"),
            );
        }

        // `on_stop` is the meaning of an agent stopping without a check, so it
        // means nothing unless an agent stopping is what resolves the pipeline.
        if pipeline.on_stop.is_some() && pipeline.await_on != Some(Await::AgentStopped) {
            problems.push(
                Problem::new(
                    &at,
                    "on_stop is set but await is not \"agent_stopped\", so no stop ever resolves it",
                )
                .hint("set await = \"agent_stopped\", or drop on_stop"),
            );
        }
        if pipeline.on_stop == Some(Outcome::Started) {
            problems.push(
                Problem::new(
                    &at,
                    "on_stop = \"started\" would leave the step waiting for the wait that just ended",
                )
                .hint("use pass, reject or error"),
            );
        }

        // Rule: no await = "human" inside a loop, or the loop asks you N times.
        if pipeline.loops() && pipeline.await_on == Some(Await::Human) {
            problems.push(
                Problem::new(
                    &at,
                    "await = \"human\" in a pipeline that loops would ask you again every round",
                )
                .hint("split the human step into a pipeline of its own"),
            );
        }
        if pipeline.loops() {
            // A task records one round, scoped to the innermost
            // pipeline. Descending into a nested pipeline from a looping one
            // would overwrite the round the loop is counting, so the two cannot
            // be combined.
            for step in pipeline.steps.iter().chain(pipeline.on_fail.iter()) {
                if let Some(inner) = policy.nested_pipeline(step) {
                    problems.push(
                        Problem::new(
                            &at,
                            format!(
                                "this pipeline loops and step {step:?} is a pipeline: entering \
                                 {inner:?} would reset the round the loop is counting"
                            ),
                        )
                        .hint("flatten the nested steps into this pipeline, or drop the loop"),
                    );
                }
            }

            // on_fail is one of this pipeline's steps too, so it is checked with them.
            for step in pipeline.steps.iter().chain(pipeline.on_fail.iter()) {
                if policy
                    .nested_pipeline(step)
                    .and_then(|inner| config.pipeline.get(inner))
                    .is_some_and(|inner| inner.await_on == Some(Await::Human))
                {
                    problems.push(Problem::new(
                        &at,
                        format!(
                            "this pipeline loops and step {step:?} awaits a human, \
                             so it would ask you up to {} times",
                            pipeline.max_rounds.unwrap_or(0)
                        ),
                    ));
                }
            }
        }
    }

    problems.extend(composition_problems(policy));

    // Rule: every pipeline named by a type exists.
    for (name, task_type) in &config.types {
        let at = format!("type.{name}");
        if task_type.description.trim().is_empty() {
            problems.push(
                Problem::new(&at, "description is empty")
                    .hint("it is the only thing an agent has to choose by"),
            );
        }
        if task_type.pipelines.is_empty() {
            problems.push(Problem::new(&at, "no pipelines, so this type does nothing"));
        }
        // `task.pipeline` records a position by name too.
        for duplicate in duplicates(&task_type.pipelines) {
            problems.push(
                Problem::new(
                    &at,
                    format!("pipeline {duplicate:?} appears more than once"),
                )
                .hint("a task records its position by pipeline name, so names must be unique"),
            );
        }
        for pipeline in &task_type.pipelines {
            if !config.pipeline.contains_key(pipeline) {
                let known = config.pipeline.keys().cloned().collect::<Vec<_>>();
                problems.push(
                    Problem::new(&at, format!("no such pipeline {pipeline:?}"))
                        .hint(format!("defined pipelines: {}", list(&known))),
                );
            }
        }
    }

    problems
}

/// Rule: no cycles in pipeline composition, and nesting depth capped at 2.
fn composition_problems(policy: &Policy) -> Vec<Problem> {
    let config = &policy.config;
    let mut problems = Vec::new();

    // Cycles first, so a cycle is reported as one rather than as depth.
    let mut cycles: BTreeSet<Vec<String>> = BTreeSet::new();
    for start in config.pipeline.keys() {
        let mut path = Vec::new();
        find_cycles(policy, start, &mut path, &mut cycles);
    }
    for cycle in &cycles {
        let at = format!("pipeline.{}", cycle[0]);
        problems.push(
            Problem::new(
                &at,
                format!("pipeline composition is a cycle: {}", cycle.join(" -> ")),
            )
            .hint("a type is the only place a sequence may be composed, and it may not loop"),
        );
    }
    if !cycles.is_empty() {
        return problems;
    }

    // Depth: a pipeline used as a step may contain only scripts.
    for (name, pipeline) in &config.pipeline {
        for step in &pipeline.steps {
            let Some(inner) = policy
                .nested_pipeline(step)
                .and_then(|n| config.pipeline.get(n))
            else {
                continue;
            };
            let deeper: Vec<String> = inner
                .steps
                .iter()
                .filter(|s| policy.nested_pipeline(s).is_some())
                .cloned()
                .collect();
            if !deeper.is_empty() {
                problems.push(
                    Problem::new(
                        format!("pipeline.{name}"),
                        format!(
                            "step {step:?} is a pipeline that itself composes {} — nesting is \
                             capped at 2",
                            list(&deeper)
                        ),
                    )
                    .hint("flatten one of the levels"),
                );
            }
        }
    }
    problems
}

fn find_cycles(
    policy: &Policy,
    at: &str,
    path: &mut Vec<String>,
    found: &mut BTreeSet<Vec<String>>,
) {
    if let Some(start) = path.iter().position(|p| p == at) {
        // Report the cycle from its own smallest name, so A->B->A and B->A->B
        // are one problem rather than two.
        let mut cycle: Vec<String> = path[start..].to_vec();
        if let Some(offset) = cycle
            .iter()
            .enumerate()
            .min_by_key(|(_, name)| name.as_str())
            .map(|(i, _)| i)
        {
            cycle.rotate_left(offset);
        }
        cycle.push(cycle[0].clone());
        found.insert(cycle);
        return;
    }
    let Some(pipeline) = policy.config.pipeline.get(at) else {
        return;
    };
    path.push(at.to_string());
    for step in &pipeline.steps {
        if let Some(inner) = policy.nested_pipeline(step) {
            let inner = inner.to_string();
            find_cycles(policy, &inner, path, found);
        }
    }
    path.pop();
}

/// The rule both `steps` and `on_fail` obey: the name must resolve to something
/// runnable.
fn unresolved(policy: &Policy, at: &str, name: &str, role: &str) -> Vec<Problem> {
    if policy.step_kind(name).is_some() {
        return Vec::new();
    }
    let searched = policy
        .script_dirs
        .iter()
        .map(|d| format!("{}/{name}.sh", d.display()))
        .collect::<Vec<_>>()
        .join(", ");
    let problem = Problem::new(
        at,
        format!("{role} {name:?} is neither a pipeline nor a runnable script"),
    )
    .hint(format!("looked for {searched} — is it executable?"));
    vec![problem]
}

fn duplicates(names: &[String]) -> Vec<String> {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for name in names {
        *seen.entry(name.as_str()).or_default() += 1;
    }
    seen.into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(name, _)| name.to_string())
        .collect()
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

/// The message a person reads when their config is wrong.
pub fn report(policy: &Policy, problems: &[Problem]) -> String {
    let mut out = format!(
        "{} has {} problem{}:",
        policy.path.display(),
        problems.len(),
        if problems.len() == 1 { "" } else { "s" }
    );
    for problem in problems {
        out.push_str(&format!("\n  {}: {}", problem.at, problem.message));
        if let Some(hint) = &problem.hint {
            out.push_str(&format!("\n    {hint}"));
        }
    }
    out
}

/// Steps grouped by what they turned out to mean. For `shep validate` output.
pub fn resolved_steps(policy: &Policy, pipeline: &str) -> Vec<(String, Option<StepKind>)> {
    policy
        .config
        .pipeline
        .get(pipeline)
        .map(|p| {
            p.steps
                .iter()
                .map(|s| (s.clone(), policy.step_kind(s)))
                .collect()
        })
        .unwrap_or_default()
}

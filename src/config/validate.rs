//! The config validation rules, plus three the data model forces.
//!
//! Every rule reports rather than returns, so one pass tells you everything
//! that is wrong with a config instead of the first thing.

use super::{Policy, SIGNAL_AGENT_STOPPED, Step, StepKind};
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

        let step_names: Vec<String> = pipeline.steps.iter().map(|s| s.name().to_string()).collect();

        // `task.step` records a step by name, so a name appearing twice
        // in one pipeline leaves the engine unable to say where it is.
        for duplicate in duplicates(&step_names) {
            problems.push(
                Problem::new(&at, format!("step {duplicate:?} appears more than once"))
                    .hint("a task records its position by step name, so names must be unique here"),
            );
        }

        // Rule: every step resolves to an executable file, or to another pipeline.
        for entry in &pipeline.steps {
            let step = entry.name();
            problems.extend(unresolved(policy, &at, step, "step"));
            problems.extend(await_problems(policy, &at, entry));

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

        if pipeline.loops() {
            // A task records one round, scoped to the innermost
            // pipeline. Descending into a nested pipeline from a looping one
            // would overwrite the round the loop is counting, so the two cannot
            // be combined.
            for step in pipeline.step_names() {
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
        for step in pipeline.step_names() {
            let Some(inner) = policy
                .nested_pipeline(step)
                .and_then(|n| config.pipeline.get(n))
            else {
                continue;
            };
            let deeper: Vec<String> = inner
                .steps
                .iter()
                .filter(|s| policy.nested_pipeline(s.name()).is_some())
                .map(|s| s.name().to_string())
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
    for step in pipeline.step_names() {
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

/// Everything wrong with one step's `await` / `on_missing`.
fn await_problems(policy: &Policy, at: &str, step: &Step) -> Vec<Problem> {
    let mut problems = Vec::new();
    let name = step.name();

    if let Some(signal) = step.await_on() {
        // Rule: await must name a signal shepherd knows — a built-in or a
        // declared `[signal.*]`. A typo here is a step that waits forever.
        if !policy.signal_known(signal) {
            problems.push(
                Problem::new(
                    at,
                    format!("step {name:?} awaits unknown signal {signal:?}"),
                )
                .hint(format!(
                    "known signals: {}. Declare a custom one with [signal.{signal}]",
                    list(&policy.known_signals())
                )),
            );
        }
    }

    // Rule: on_missing is the meaning of resolving without a check, so it needs
    // something to resolve — i.e. an await.
    if step.on_missing().is_some() && step.await_on().is_none() {
        problems.push(
            Problem::new(at, format!("step {name:?} sets on_missing but not await"))
                .hint("on_missing is what a resolution with no check means; give the step an await"),
        );
    }
    // on_missing is only consulted when a stop leaves no verdict, which is the
    // agent_stopped case; a human or a custom signal supplies its own verdict.
    if step.on_missing().is_some()
        && step.await_on().is_some()
        && step.await_on() != Some(SIGNAL_AGENT_STOPPED)
    {
        problems.push(
            Problem::new(
                at,
                format!("step {name:?} sets on_missing but does not await \"agent_stopped\""),
            )
            .hint("on_missing only applies to an agent stopping without a check"),
        );
    }
    if step.on_missing() == Some(Outcome::Started) {
        problems.push(
            Problem::new(
                at,
                format!("step {name:?} on_missing = \"started\" is not a settled outcome"),
            )
            .hint("use pass, reject or error"),
        );
    }

    // timeout only means something for a step that waits, and must be readable.
    if let Some(raw) = step.timeout() {
        if step.await_on().is_none() {
            problems.push(
                Problem::new(at, format!("step {name:?} sets timeout but not await"))
                    .hint("a timeout bounds a wait; give the step an await, or drop the timeout"),
            );
        }
        if super::parse_duration(raw).is_none() {
            problems.push(
                Problem::new(at, format!("step {name:?} has an unreadable timeout {raw:?}"))
                    .hint("use a number of seconds, or a unit: 90s, 30m, 2h, 1d"),
            );
        }
    }
    if step.on_timeout().is_some() && step.timeout().is_none() {
        problems.push(
            Problem::new(at, format!("step {name:?} sets on_timeout but not timeout"))
                .hint("on_timeout is the verdict a timeout fires; give the step a timeout"),
        );
    }
    if step.on_timeout() == Some(Outcome::Started) {
        problems.push(
            Problem::new(
                at,
                format!("step {name:?} on_timeout = \"started\" is not a settled outcome"),
            )
            .hint("use pass, reject or error"),
        );
    }
    problems
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
                .map(|s| (s.name().to_string(), policy.step_kind(s.name())))
                .collect()
        })
        .unwrap_or_default()
}

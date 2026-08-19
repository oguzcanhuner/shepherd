//! Config acceptance: every validation rule has a test, and a broken config yields a
//! usable error rather than a panic.

mod common;

use common::{Repo, make_executable};
use shepherd::config::{Policy, StepKind};

/// The example config from docs/configuration.md, which must parse and validate as written.
const EXAMPLE_CONFIG: &str = r#"
[pipeline.implement]
steps = [{ run = "code", await = "agent_stopped" }]

[pipeline.review]
steps        = ["lint", "test", "agent_review"]
on_fail      = "fix"
max_rounds   = 3
on_exhausted = "reject"

[pipeline.handoff]
steps = ["show_diff"]

[pipeline.integrate]
steps = ["integrate"]

[type.feature]
description = "Normal change. Reviewed, then shown to you."
pipelines   = ["implement", "review", "handoff", "integrate"]

[type.hotfix]
description = "Urgent production fix. No review, no handoff."
pipelines   = ["implement", "integrate"]
"#;

fn plan_repo() -> Repo {
    let repo = Repo::new();
    for step in [
        "code",
        "lint",
        "test",
        "agent_review",
        "fix",
        "show_diff",
        "integrate",
    ] {
        repo.script(step);
    }
    repo
}

#[test]
fn the_config_from_the_plan_is_valid() {
    let repo = plan_repo();
    let policy = repo
        .load(EXAMPLE_CONFIG)
        .expect("the plan's own example must validate");

    assert_eq!(policy.config.pipeline.len(), 4);
    assert_eq!(
        policy.step_await("implement", "code"),
        Some("agent_stopped")
    );
    assert_eq!(policy.step_await("handoff", "show_diff"), None);
    assert_eq!(policy.config.pipeline["review"].max_rounds, Some(3));
    assert_eq!(
        policy.config.pipeline["review"].on_exhausted,
        Some(shepherd::Outcome::Reject)
    );
    // Absent await means synchronous.
    assert_eq!(policy.step_await("integrate", "integrate"), None);

    let feature = policy.task_type("feature").expect("type feature");
    assert_eq!(feature.pipelines.len(), 4);
    assert!(feature.description.contains("Reviewed"));
}

#[test]
fn steps_resolve_to_scripts_by_filename() {
    let repo = plan_repo();
    let policy = repo.load(EXAMPLE_CONFIG).expect("valid");

    match policy.step_kind("lint") {
        Some(StepKind::Script(path)) => {
            assert!(path.ends_with(".shep/scripts/lint.sh"), "got {path:?}");
        }
        other => panic!("expected a script, got {other:?}"),
    }
    assert_eq!(policy.step_kind("nonexistent"), None);
}

#[test]
fn a_pipeline_may_be_used_as_a_step() {
    let repo = Repo::new();
    repo.script("lint").script("code");
    let policy = repo
        .load(
            r#"
[pipeline.check]
steps = ["lint"]

[pipeline.work]
steps = ["code", "check"]

[type.feature]
description = "compose a pipeline as a step"
pipelines = ["work"]
"#,
        )
        .expect("a pipeline is a legal step: it returns an outcome");

    assert_eq!(
        policy.step_kind("check"),
        Some(StepKind::Pipeline("check".into()))
    );
}

// ---------------------------------------------------------------- §8 rules

#[test]
fn rule_a_step_must_resolve_to_something() {
    let repo = Repo::new();
    repo.script("lint");
    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint", "typecheck"]

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    );
    assert!(problems.contains("\"typecheck\""), "got {problems}");
    assert!(
        problems.contains("neither a pipeline nor a runnable script"),
        "got {problems}"
    );
    // The error says where it looked, which is the only way to fix it quickly.
    assert!(
        problems.contains(".shep/scripts/typecheck.sh"),
        "got {problems}"
    );
}

#[test]
fn rule_a_step_script_must_be_executable() {
    let repo = Repo::new();
    repo.unrunnable_script("lint");
    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint"]

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    );
    assert!(problems.contains("is it executable?"), "got {problems}");
}

#[test]
fn rule_a_step_name_may_not_mean_two_things() {
    let repo = Repo::new();
    repo.script("review").script("lint");
    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint"]

[pipeline.work]
steps = ["review"]

[type.feature]
description = "x"
pipelines = ["work"]
"#,
    );
    assert!(
        problems.contains("names a pipeline, but the script"),
        "got {problems}"
    );
    assert!(
        problems.contains("a script of the same name always wins"),
        "got {problems}"
    );
}

#[test]
fn rule_a_type_may_only_name_pipelines_that_exist() {
    let repo = Repo::new();
    repo.script("lint");
    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint"]

[type.feature]
description = "x"
pipelines = ["review", "handoff"]
"#,
    );
    assert!(
        problems.contains("no such pipeline \"handoff\""),
        "got {problems}"
    );
    // And it lists what does exist.
    assert!(
        problems.contains("defined pipelines: review"),
        "got {problems}"
    );
}

#[test]
fn rule_on_fail_names_a_step_of_this_pipelines_machine() {
    let repo = Repo::new();
    repo.script("lint").script("fix");

    // Naming it in on_fail is what makes `fix` a step of this pipeline. It is
    // deliberately absent from `steps`: a repair step that ran in the forward
    // sequence would run when nothing was wrong. This is the shape of the
    // example review pipeline.
    repo.load(
        r#"
[pipeline.review]
steps = ["lint"]
on_fail = "fix"
max_rounds = 2

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    )
    .expect("a repair step outside the forward sequence is the normal shape");

    // What is not allowed is naming something that does not resolve at all.
    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint"]
on_fail = "repair"
max_rounds = 2

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    );
    assert!(
        problems.contains("on_fail \"repair\" is neither a pipeline nor a runnable script"),
        "got {problems}"
    );
}

#[test]
fn rule_on_fail_without_max_rounds_is_an_unbounded_loop() {
    let repo = Repo::new();
    repo.script("lint").script("fix");
    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint", "fix"]
on_fail = "fix"

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    );
    assert!(problems.contains("unbounded loop"), "got {problems}");
    assert!(problems.contains("max_rounds = 3"), "got {problems}");
}

#[test]
fn rule_a_cap_with_nothing_to_loop_is_dead_config() {
    let repo = Repo::new();
    repo.script("lint");
    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint"]
max_rounds = 3

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    );
    assert!(problems.contains("nothing loops"), "got {problems}");

    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint"]
on_exhausted = "reject"

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    );
    assert!(problems.contains("can never be spent"), "got {problems}");
}

#[test]
fn rule_await_must_name_a_known_signal() {
    let repo = Repo::new();
    repo.script("lint");
    let problems = repo.problems(
        r#"
[pipeline.review]
steps = [{ run = "lint", await = "agent_finished" }]

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    );
    // An unknown signal names the alternatives, and says how to add a custom one.
    assert!(problems.contains("unknown signal"), "got {problems}");
    assert!(problems.contains("agent_stopped"), "got {problems}");

    // The built-in signal is accepted.
    let repo = Repo::new();
    repo.script("lint");
    repo.load(
        r#"
[pipeline.review]
steps = [{ run = "lint", await = "agent_stopped" }]

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    )
    .expect("agent_stopped must be legal");

    // A declared custom signal is accepted too.
    let repo = Repo::new();
    repo.script("lint");
    repo.load(
        r#"
[signal.ci]
description = "GitHub Actions result"

[pipeline.review]
steps = [{ run = "lint", await = "ci" }]

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    )
    .expect("a declared signal must be awaitable");
}

#[test]
fn rule_pipeline_composition_may_not_cycle() {
    let repo = Repo::new();
    repo.script("lint").script("fix");

    // Direct self-reference.
    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint", "review"]

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    );
    assert!(problems.contains("cycle"), "got {problems}");

    // And the indirect kind.
    let problems = repo.problems(
        r#"
[pipeline.a]
steps = ["lint", "b"]

[pipeline.b]
steps = ["fix", "a"]

[type.feature]
description = "x"
pipelines = ["a"]
"#,
    );
    assert!(problems.contains("a -> b -> a"), "got {problems}");
    // Reported once, from its smallest name, not once per participant.
    assert_eq!(problems.matches("is a cycle").count(), 1, "got {problems}");
}

#[test]
fn rule_nesting_is_capped_at_two() {
    let repo = Repo::new();
    repo.script("lint").script("fix").script("code");
    let problems = repo.problems(
        r#"
[pipeline.inner]
steps = ["lint"]

[pipeline.middle]
steps = ["fix", "inner"]

[pipeline.outer]
steps = ["code", "middle"]

[type.feature]
description = "x"
pipelines = ["outer"]
"#,
    );
    assert!(
        problems.contains("nesting is capped at 2"),
        "got {problems}"
    );
}

#[test]
fn rule_unknown_keys_are_an_error() {
    let repo = Repo::new();
    repo.script("lint");

    // A typo in a pipeline field.
    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint"]
max_round = 3

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    );
    assert!(problems.contains("max_round"), "got {problems}");

    // A typo in a section name is caught the same way, rather than defining a
    // pipeline nobody will ever run.
    let problems = repo.problems(
        r#"
[pipelines.review]
steps = ["lint"]

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    );
    assert!(problems.contains("pipelines"), "got {problems}");
}

// ------------------------------------------------- usable errors, not panics

#[test]
fn a_pipeline_with_no_steps_is_rejected() {
    let repo = Repo::new();
    let problems = repo.problems(
        r#"
[pipeline.review]
steps = []

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    );
    assert!(problems.contains("does nothing"), "got {problems}");
}

#[test]
fn positions_are_names_so_names_must_be_unique() {
    let repo = Repo::new();
    repo.script("lint").script("fix");

    // A task records its step by name, so a repeat leaves the engine
    // unable to say where it is.
    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint", "fix", "lint"]

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    );
    assert!(
        problems.contains("step \"lint\" appears more than once"),
        "got {problems}"
    );

    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint"]

[type.feature]
description = "x"
pipelines = ["review", "review"]
"#,
    );
    assert!(
        problems.contains("pipeline \"review\" appears more than once"),
        "got {problems}"
    );
}

#[test]
fn a_type_needs_a_description_because_that_is_what_is_chosen_by() {
    let repo = Repo::new();
    repo.script("lint");
    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint"]

[type.feature]
pipelines = ["review"]
"#,
    );
    assert!(problems.contains("description"), "got {problems}");

    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint"]

[type.feature]
description = "   "
pipelines = ["review"]
"#,
    );
    assert!(
        problems.contains("only thing an agent has to choose by"),
        "got {problems}"
    );
}

#[test]
fn a_config_with_no_types_is_rejected() {
    let repo = Repo::new();
    repo.script("lint");
    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint"]
"#,
    );
    assert!(problems.contains("no types defined"), "got {problems}");
}

#[test]
fn every_problem_is_reported_at_once() {
    let repo = Repo::new();
    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint", "missing"]
on_fail = "nowhere"

[type.feature]
description = "x"
pipelines = ["review", "absent"]
"#,
    );
    // Fixing these one error message at a time would be misery, so they arrive
    // together.
    let count: usize = problems
        .split_whitespace()
        .skip_while(|word| *word != "has")
        .nth(1)
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("expected a problem count in {problems}"));
    assert!(count >= 4, "got {problems}");
    assert!(problems.contains("\"lint\""), "got {problems}");
    assert!(problems.contains("\"missing\""), "got {problems}");
    assert!(problems.contains("\"nowhere\""), "got {problems}");
    assert!(problems.contains("\"absent\""), "got {problems}");
}

#[test]
fn broken_toml_reports_where_it_broke() {
    let repo = Repo::new();
    let problems = repo.problems("[pipeline.review\nsteps = [\"lint\"]\n");
    assert!(!problems.is_empty(), "a parse error must be reported");
    assert!(
        problems.contains("line 1") || problems.contains("expected"),
        "got {problems}"
    );
}

#[test]
fn a_missing_config_says_what_is_missing_and_where() {
    let dir = tempfile::tempdir().expect("temp dir");
    let err = Policy::load(dir.path()).expect_err("no config");
    let message = err.to_string();
    assert!(message.contains(".shep/config.toml"), "got {message}");
    assert!(message.contains("policy in the repo"), "got {message}");
}

#[test]
fn an_unknown_type_returns_the_menu() {
    let repo = plan_repo();
    let policy = repo.load(EXAMPLE_CONFIG).expect("valid");

    let err = policy.task_type("refactor").expect_err("no such type");
    let message = err.to_string();
    assert!(
        message.contains("unknown type \"refactor\""),
        "got {message}"
    );
    // The menu, because the agent asking is the one that has to choose again.
    assert!(message.contains("feature"), "got {message}");
    assert!(message.contains("Normal change"), "got {message}");
    assert!(message.contains("hotfix"), "got {message}");
    assert!(message.contains("Urgent production fix"), "got {message}");
}

#[test]
fn on_exhausted_may_not_be_a_promise() {
    let repo = Repo::new();
    repo.script("lint").script("fix");
    let problems = repo.problems(
        r#"
[pipeline.review]
steps = ["lint", "fix"]
on_fail = "fix"
max_rounds = 2
on_exhausted = "started"

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    );
    assert!(
        problems.contains("not an outcome a pipeline can have"),
        "got {problems}"
    );
}

#[test]
fn a_home_fallback_covers_project_agnostic_scripts() {
    // ~/.config/shep/scripts is the fallback search path.
    let home = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(home.path().join(".config/shep/scripts")).expect("mkdir");
    let shared = home.path().join(".config/shep/scripts/lint.sh");
    std::fs::write(&shared, "#!/usr/bin/env bash\n").expect("write");
    make_executable(&shared);

    let repo = Repo::new();
    let path = repo.write(
        r#"
[pipeline.review]
steps = ["lint"]

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    );
    let text = std::fs::read_to_string(&path).expect("read");

    // The search path is a parameter, not ambient state. Reaching for the real
    // HOME here would mean mutating it process-wide, and the other tests in this
    // binary resolve script paths at the same time — two of them expect `lint`
    // *not* to resolve, so they would fail whenever the swap overlapped them.
    let policy = Policy::parse_in(
        &text,
        repo.root(),
        &path,
        vec![
            repo.root().join(".shep/scripts"),
            home.path().join(".config/shep/scripts"),
        ],
    )
    .expect("a script in the fallback path resolves");

    match policy.step_kind("lint") {
        Some(StepKind::Script(p)) => assert!(p.starts_with(home.path()), "got {p:?}"),
        other => panic!("expected the shared script, got {other:?}"),
    }
}

#[test]
fn the_repo_wins_over_the_shared_fallback() {
    // Both exist: the repo's own script is the one that judges its code.
    let home = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(home.path().join(".config/shep/scripts")).expect("mkdir");
    let shared = home.path().join(".config/shep/scripts/lint.sh");
    std::fs::write(&shared, "#!/usr/bin/env bash\n").expect("write");
    make_executable(&shared);

    let repo = Repo::new();
    repo.script("lint");
    let path = repo.write(
        r#"
[pipeline.review]
steps = ["lint"]

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    );
    let text = std::fs::read_to_string(&path).expect("read");

    let policy = Policy::parse_in(
        &text,
        repo.root(),
        &path,
        vec![
            repo.root().join(".shep/scripts"),
            home.path().join(".config/shep/scripts"),
        ],
    )
    .expect("valid");

    match policy.step_kind("lint") {
        Some(StepKind::Script(p)) => assert!(p.starts_with(repo.root()), "got {p:?}"),
        other => panic!("expected the repo's script, got {other:?}"),
    }
}

#[test]
fn rule_a_looping_pipeline_may_not_nest_another() {
    let repo = Repo::new();
    repo.script("lint").script("fix").script("deep");

    // A task records one round, scoped to the innermost pipeline.
    // Descending would overwrite the round the loop is counting.
    let problems = repo.problems(
        r#"
[pipeline.inner]
steps = ["deep"]

[pipeline.review]
steps = ["lint", "inner"]
on_fail = "fix"
max_rounds = 3

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    );
    assert!(
        problems.contains("would reset the round the loop is counting"),
        "got {problems}"
    );

    // Without the loop, the same nesting is fine.
    repo.load(
        r#"
[pipeline.inner]
steps = ["deep"]

[pipeline.review]
steps = ["lint", "inner"]

[type.feature]
description = "x"
pipelines = ["review"]
"#,
    )
    .expect("nesting is only a problem where a round is being counted");
}

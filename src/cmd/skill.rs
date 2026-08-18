use crate::config::Policy;
use anyhow::Result;
use std::path::Path;

/// `shep skill` — everything a conversational agent needs to act as an
/// orchestrator: when to create a task, how to write a brief, how to watch
/// tasks and settle handoffs.
///
/// `shep skill --authoring` is the other role: everything an agent needs to
/// *write* a repository's workflow — the config schema, the script contract,
/// and the design rules the validator enforces.
///
/// The output is markdown, meant to be loaded into an agent's context (a
/// skill file, a CLAUDE.md section, or pasted into a conversation). The type
/// menu is read live from the repo's config so the skill never goes stale.
pub fn run(repo: &Path, authoring: bool) -> Result<()> {
    if authoring {
        println!("{}", AUTHORING.trim_start());
        return Ok(());
    }
    println!("{}", SKILL.trim_start());

    // The live part: this repo's actual menu. A broken or missing config is
    // reported inline rather than failing the whole skill, so the static
    // guidance still loads.
    println!("## Task types in this repository\n");
    match Policy::load(repo) {
        Ok(policy) => {
            for (name, t) in &policy.config.types {
                println!("- `{name}` — {}", t.description);
                println!("  - pipelines: {}", t.pipelines.join(" → "));
            }
            println!();
            println!(
                "Re-run `shep types` before creating a task if this conversation has \
                 been long-lived; the menu can change."
            );
        }
        Err(e) => {
            println!(
                "Could not read this repository's `.shep/config.toml` ({e}).\n\
                 Run `shep types` from the repository root to see the menu, and \
                 `shep validate` to see what is wrong with the config."
            );
        }
    }
    Ok(())
}

const SKILL: &str = r#"
# Orchestrating work with shepherd

You are talking to a person who will sometimes ask for changes to their code.
Shepherd turns such a request into a task and runs it through the pipelines
configured for this repository — spawning coding agents, running lint, tests
and review, and pausing for the person's approval where the configuration says
to. Your job is at the edges: deciding when a request should become a task,
creating it well, watching it, and settling it when it waits for a decision.

Every `shep` command is a local command; there is nothing to connect to.

## When to create a task

Create a task when the person asks for a concrete change to the code: a
feature, a fix, a refactor. Do the work yourself, without shepherd, when the
request is a question, a one-line tweak they want to see immediately, or
exploration that has no definition of done.

## Creating a task

1. Run `shep types` to see the menu. Each type is a named workflow with a
   description written by the person; pick the one whose description fits.
   If none fits, ask rather than guess.
2. Write the brief. The brief is the only instruction the working agent gets,
   so make it complete on its own:
   - what to change and where, with file paths when you know them
   - what "done" looks like — observable behaviour, not implementation detail
   - constraints worth stating: compatibility, style, things not to touch
   Keep it to a paragraph or two. The working agent can read the code; it
   cannot read this conversation.
3. Create it:

       shep create --type <type> "<brief>"

   The command prints the task id and nothing else. Report the id to the
   person.

## Watching tasks

    shep ps                  # every active task and where it is
    shep get <task>          # one task in full
    shep trace <task>        # the task's history, as a tree
    shep status              # is the supervisor running; is the config valid

A task moves on its own. You do not need to poll on the person's behalf, but
when they ask how something is going, `shep ps` is the answer, and
`shep trace <task>` explains anything surprising.

## When a task waits for a decision

Some pipelines pause and wait for a human. The task shows as waiting in
`shep ps`, and typically a pane has opened showing the change. The decision
belongs to the person, and only relay it:

    shep approve --task <task> --note "why"
    shep reject  --task <task> --note "why"

The note goes on the record. On reject, the task goes wherever this
repository's configuration sends rejections.

## When a task is parked

A parked task had a step break — a script error, not a negative verdict.

    shep trace <task>        # shows which step broke and why
    shep retry <task>        # run that step again
    shep cancel <task> --reason "why"   # give up on it

Read the trace before retrying: if the step will break the same way again,
fix the cause first or tell the person.

## Other verbs

    shep run <pipeline> --task <task>   # send a task through a pipeline by hand
    shep pause | shep resume            # stop and restart the supervisor's intake
    shep validate                       # check the repo's config and list problems

## What not to do

- Do not do the task's work yourself in parallel; the task has its own working
  copy and its own agent, and two writers will diverge.
- Do not approve or reject on your own judgement; relay the person's decision.
- Do not edit `.shep/config.toml` to change a workflow mid-task.
"#;

const AUTHORING: &str = r#"
# Authoring shepherd workflows

You are helping a person define how work moves through their repository.
A workflow lives in two places, both versioned with the code:

- `.shep/config.toml` — the shape: task types, pipelines, retry rules, waits.
- `.shep/scripts/` — the work: one script per step, any language.

Start from a scaffold if the repo has nothing yet: `shep init` creates a
minimal valid config and stub scripts without overwriting anything. After
every edit, run `shep validate` — it lists every problem at once, with hints.

## The model

- A **type** is an ordered list of pipelines. It is what a task is created as,
  and it cannot loop, so a task always terminates.
- A **pipeline** is an ordered list of steps with its own retry rules. It
  reports one outcome, which is why a pipeline's name can appear as a step in
  another pipeline (nesting is capped at two levels).
- A **step** names a script: step `lint` runs `.shep/scripts/lint.sh`.

## Config schema

    [pipeline.<name>]
    steps        = ["a", "b"]   # required; scripts or other pipelines, in order
    on_fail      = "fix"        # optional: step in THIS pipeline run after a reject
    max_rounds   = 3            # required whenever on_fail is set
    on_exhausted = "reject"     # the pipeline's outcome when max_rounds is spent
    await        = "human"      # optional: "human" or "agent_stopped"
    on_stop      = "pass"       # optional, agent_stopped only: what a stop with
                                # no check means (default: error)

    [type.<name>]
    description = "Shown in `shep types`; write it for the person choosing."
    pipelines   = ["implement", "review", "handoff", "integrate"]

Unknown fields are errors, not warnings — a typo cannot pass silently.

Retry semantics: when a step reports `reject`, the pipeline runs `on_fail`,
then starts its steps again from the top, with the round counter one higher.
`await` semantics: a step that reports `started` leaves the pipeline waiting —
for `shep approve`/`shep reject` when `await = "human"`, or for the task's
pane agent to stop when `await = "agent_stopped"`.

## The script contract

A script runs in the task's working copy with these variables:

    SHEP_TASK_ID  SHEP_TYPE   SHEP_PIPELINE  SHEP_STEP  SHEP_ROUND
    SHEP_WORKTREE SHEP_BRANCH SHEP_BASE      SHEP_REPO
    SHEP_PANE     # when a pane is already bound
    SHEP_DB       # so `shep` subcommands find the store

Everything it prints is kept as the step's log, except the FINAL line, which
must be one JSON object:

    {"outcome": "pass",    "note": "optional"}   # move forward
    {"outcome": "reject",  "note": "why"}        # a verdict; go to on_fail
    {"outcome": "started", "pane": "wA:p2"}      # an agent will finish later
    {"outcome": "error",   "note": "why"}        # broken; park until `shep retry`

A non-zero exit or an unparseable last line counts as `error`. Distinguish
carefully between `reject` (the work is wrong) and `error` (the step is
broken): rejects loop through `on_fail`, errors stop the task.

## Steps that launch an agent

A step in a pipeline with `await = "agent_stopped"` opens a pane, binds it,
starts an agent, and reports `started`:

    pane=$(herdr pane split --json | jq -r .pane_id)
    shep bind-pane "$pane"
    herdr agent start "$pane" -- claude --permission-mode acceptEdits
    herdr agent prompt "$pane" "Run \`shep context\` for your brief, then implement it."
    echo '{"outcome":"started","pane":"'"$pane"'"}'

The prompt is what differentiates steps: an implement step says "implement",
a review step says "review and record a verdict". When the pipeline resolves,
its result comes from the latest check recorded for that step; when there is
no check, from the pipeline's `on_stop`; and with neither, it is an error.

Use `on_stop = "pass"` for producing pipelines (implement): the agent just
works and stops, and the next pipeline judges the result. Leave it unset for
judging pipelines (review): tell that agent to run `shep check submit --pass`
or `--fail` (body on stdin) before it stops, because a reviewer stopping
without a verdict is a failure. A recorded check always wins over `on_stop`.

## Design rules

- Make each step do one thing, so `shep trace` reads as a story.
- `on_fail` must name a step in the same pipeline; rounds are scoped there.
- Put human gates (`await = "human"`) exactly where a person would want to
  look, typically after review and before integration — not everywhere.
- Verdicts about code belong in checks, not in notes: `shep check submit`
  pins them to the exact commit judged, which is what an integrate step
  should verify before merging.
- Scripts must be executable (`chmod +x`) and print their JSON as the last
  line, whatever else they do.
- After any change: `shep validate`, then exercise it with a cheap task
  (`shep create --type <type> "try the workflow"`) before relying on it.
"#;

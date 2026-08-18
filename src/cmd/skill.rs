use crate::config::Policy;
use anyhow::Result;
use std::path::Path;

/// `shep skill` — everything a conversational agent needs to act as an
/// orchestrator: when to create a task, how to write a brief, how to watch
/// tasks and settle handoffs.
///
/// The output is markdown, meant to be loaded into an agent's context (a
/// skill file, a CLAUDE.md section, or pasted into a conversation). The type
/// menu is read live from the repo's config so the skill never goes stale.
pub fn run(repo: &Path) -> Result<()> {
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

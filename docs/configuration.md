# Configuration

Shepherd's workflows are defined in `.shep/config.toml`, kept in the repository
being worked on. Each repository gets its own configuration, because the right
lint and test commands differ from project to project, and the workflow should
be versioned with the code it governs.

## Vocabulary

| Term | Meaning |
| --- | --- |
| **task** | One unit of requested work. Has a description, a type, and a current position. |
| **type** | A named, ordered list of pipelines. What the agent picks when creating a task. |
| **pipeline** | An ordered list of steps with its own retry rules and its own result. |
| **step** | One script run. The step's name is the script's filename. |
| **check** | A pass/fail verdict about a specific commit, recorded by linters, test runs, reviewing agents and humans alike. |
| **event** | A permanent log entry. `shep trace` shows a task's history from these. |

## Pipelines

A pipeline runs its steps in order. Its retry rules are its own:

```toml
[pipeline.review]
steps        = ["lint", "test", "agent_review"]
on_fail      = "fix"      # the step to run after a failure
max_rounds   = 3          # how many times that can happen
on_exhausted = "reject"   # what the pipeline reports when the limit is hit
```

- `on_fail` names a step in the same pipeline to run when a step reports a
  negative verdict. After it runs, the pipeline starts its steps again, and the
  round counter goes up by one.
- `max_rounds` caps the retries. It is required whenever `on_fail` is set.
- `on_exhausted` is what the pipeline reports when the cap is reached.

A pipeline can wait instead of finishing immediately:

```toml
[pipeline.implement]
steps = ["code"]
await = "agent_stopped"   # resolves when the agent in the task's pane stops

[pipeline.handoff]
steps = ["show_diff"]
await = "human"           # resolves when you run `shep approve` or `shep reject`
```

`await` takes one of two values, or is omitted for pipelines whose steps finish
on their own:

- `agent_stopped` — the pipeline resolves when Herdr reports that the task's
  agent has stopped. The result comes from the latest recorded check for that
  step; a missing check counts as an error.
- `human` — the pipeline resolves when you run `shep approve` or `shep reject`.
  While it waits, the task is marked as yours: agent status changes in its pane
  are logged and otherwise ignored, so you can converse freely.

Because a pipeline reports a single result, a pipeline can itself be used as a
step inside another pipeline. Nesting is capped at two levels.

## Types

A type is what an agent picks when creating a task — an ordered list of
pipelines, run start to finish:

```toml
[type.feature]
description = "Normal change. Reviewed, then shown to you."
pipelines   = ["implement", "review", "handoff", "integrate"]

[type.hotfix]
description = "Urgent fix. Straight to integration."
pipelines   = ["implement", "integrate"]
```

`description` lives on types only. It is what `shep types` prints, and what an
invalid `--type` error lists, so the agent choosing a type has a menu to read.

## Validation

The configuration is checked when loaded, and on demand with `shep validate`:

- every step resolves to an existing executable file, or to another pipeline
- every pipeline named by a type exists
- `on_fail` targets a step inside the same pipeline
- `on_fail` requires `max_rounds`
- `await` is omitted, `agent_stopped`, or `human`
- pipeline composition is free of cycles, and nesting depth is capped at 2
- a pipeline with `await = "human"` sits outside any retry loop, so a loop
  can only ask you once
- misspelled or unknown keys are reported as errors

If the configuration is broken when the supervisor starts, the supervisor stays
running, declines new work, and reports the problem through `shep status` and a
Herdr notification.

## Script lookup

`steps = ["lint"]` runs `.shep/scripts/lint.sh` in the repository. Scripts that
apply to every project can live in `~/.config/shep/scripts/`, which is searched
as a fallback.

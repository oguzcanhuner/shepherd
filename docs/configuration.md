# Configuration

Shepherd's workflows are defined in `.shep/config.toml`, kept in the repository
being worked on. Each repository gets its own configuration, because the right
lint and test commands differ from project to project, and the workflow should
be versioned with the code it governs.

## Vocabulary

| Term | Meaning |
| --- | --- |
| **task** | One unit of requested work. Has a description, a type, a plan, and a current position. |
| **type** | A named, ordered list of pipelines that *seeds* a task's plan. What the agent picks. |
| **pipeline** | An ordered list of steps with its own retry rules and its own result. |
| **step** | One script run, which may also say how it defers. The step's name is the script's filename. |
| **signal** | A named event that resolves a deferred step. `agent_stopped` is built in; others are declared. |
| **check** | A pass/fail verdict about a specific commit, recorded by linters, test runs, reviewing agents and humans alike. |
| **event** | A permanent log entry. `shep trace` shows a task's history from these. |

## Steps

A pipeline runs its steps in order. A step is written either as a bare name — a
synchronous script — or as a table that also says how it completes:

```toml
[pipeline.review]
steps = [
  "lint",                                              # synchronous
  "test",                                              # synchronous
  { run = "agent_review", await = "agent_stopped" },   # defers to an agent
]
```

The name is the step's identity: a task records its position by step name,
`on_fail` targets a step by name, and the planner compares by name. The table
form only adds *how the step completes* alongside that identity.

A step's table form takes these keys:

| Key | Meaning |
| --- | --- |
| `run` | The step's name (its script, or a nested pipeline). |
| `await` | The signal that resolves the step once it reports `started`. A built-in (`agent_stopped`) or a declared `[signal.*]`. |
| `on_missing` | The verdict if the step resolves with no recorded check. Default `error`. `pass` is for work a later pipeline judges. |
| `timeout` | How long the wait may last: `90s`, `30m`, `2h`, `1d`. Only meaningful with `await`. |
| `on_timeout` | The verdict when `timeout` fires. Default `error`. |

## Pipelines

A pipeline owns its retry rules and reports one outcome:

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

Because a pipeline reports a single result, a pipeline can itself be used as a
step inside another pipeline. Nesting is capped at two levels.

## Deferring and signals

A step that launches work which finishes later reports `started` (see
[step scripts](step-scripts.md)) and waits. Its `await` names the **signal**
that resolves it:

```toml
[pipeline.build]
steps = [{ run = "code", await = "agent_stopped", on_missing = "pass" }]

[pipeline.ship]
steps = [{ run = "push_and_wait_ci", await = "ci", timeout = "30m", on_timeout = "reject" }]
```

- `agent_stopped` — the one built-in signal. Shepherd fires it when the task's
  pane agent stops (or the pane or workspace goes away). The verdict then comes
  from the latest recorded check for the step; with no check, from `on_missing`.
- **custom signals** — anything else must be declared, so a typo in `await` is
  an error rather than a step that waits forever. An external emitter fires it:

  ```toml
  [signal.ci]
  description = "GitHub Actions result, fired by shep signal"
  ```

  ```sh
  shep signal <task> --name ci --pass          # or --fail, with an optional note
  ```

  A signal carries its own verdict (`--pass`/`--fail`), recorded as a check for
  provenance.

- `timeout` bounds any wait; when it elapses the supervisor fires `on_timeout`.
  This is the backstop for a wait whose emitter never comes.

There is no `human` signal. A person is not part of the state machine — see
**Rest**, below.

## Types and the plan

A type is an ordered list of pipelines. It **seeds** a new task's plan; from
then on "what's next" lives on the task, not in the type:

```toml
[type.feature]
description = "Implemented and reviewed, then it rests for you."
pipelines   = ["implement", "review"]

[type.hotfix]
description = "Urgent fix, straight through."
pipelines   = ["implement", "integrate"]
```

`description` lives on types only. It is what `shep types` prints, and what an
invalid `--type` error lists, so the agent choosing a type has a menu to read.

`shep run <pipeline>` applies **any defined pipeline** to a task, whether or not
its type listed it — the applied pipeline joins the task's plan. This is how a
resting task is carried further.

## Rest

When a task's plan is spent it comes to **rest**: a non-terminal, idle status.
Nothing drives a resting task; it waits, often with a live pane, for a person or
the orchestrator to apply the next pipeline with `shep run` — or to leave it be.

Rest is where human interaction happens. Rather than pausing *inside* a pipeline
for approval, a workflow ends its plan at the point a person should take over;
the person looks, talks to the agent in its pane, and applies whatever pipeline
should come next. To offer both an automatic and an interactive path, compose
the pipelines differently per type — `hotfix` runs straight through to
`integrate`, `feature` stops after `review` and rests.

## Validation

The configuration is checked when loaded, and on demand with `shep validate`:

- every step resolves to an existing executable file, or to another pipeline
- every pipeline named by a type exists
- `on_fail` targets a step inside the same pipeline, and requires `max_rounds`
- `await` names a known signal — `agent_stopped` or a declared `[signal.*]`
- `on_missing` requires `await` and only applies to `agent_stopped`
- `timeout` requires `await` and must parse; `on_timeout` requires `timeout`
- `on_exhausted`, `on_missing` and `on_timeout` are settled outcomes, not `started`
- pipeline composition is free of cycles, and nesting depth is capped at 2
- misspelled or unknown keys are reported as errors

If the configuration is broken when the supervisor starts, the supervisor stays
running, declines new work, and reports the problem through `shep status` and a
Herdr notification.

## Script lookup

`steps = ["lint"]` runs `.shep/scripts/lint.sh` in the repository. Scripts that
apply to every project can live in `~/.config/shep/scripts/`, which is searched
as a fallback.

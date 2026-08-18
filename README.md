# shepherd

**Shepherd runs coding agents through workflows you define.**

You describe the workflow — which steps run, in what order, what happens when
one fails — in a small config file kept in your repository. Shepherd runs your
tasks through it: spawning coding agents in terminal panes where the work is
worth watching, running everything else as plain scripts, and pausing for your
approval exactly where you said it should.

Shepherd is a [Herdr](https://herdr.dev) plugin built around one Rust program,
`shep`.

## The idea

- **Workflows are yours.** A task type is an ordered list of pipelines; a
  pipeline is an ordered list of steps with its own retry rules. All of it lives
  in `.shep/config.toml` in your repository, versioned with the code it governs.
- **Steps are just scripts.** A step named `lint` runs `.shep/scripts/lint.sh`.
  Putting a file there is all it takes to add a step, in any language. A script
  reports its result by printing one line of JSON.
- **Every decision is written down.** Shepherd follows your configuration and
  records everything it does. The choices — what to build, what counts as
  passing, when to ship — stay with you and the scripts you wrote.

## A taste

```toml
# .shep/config.toml

[pipeline.review]
steps        = ["lint", "test", "agent_review"]
on_fail      = "fix"      # the step to run after a failure
max_rounds   = 3          # how many times that can happen
on_exhausted = "reject"   # what the pipeline reports when the limit is hit

[type.feature]
description = "Normal change. Reviewed, then shown to you."
pipelines   = ["implement", "review", "handoff", "integrate"]

[type.hotfix]
description = "Urgent fix. Straight to integration."
pipelines   = ["implement", "integrate"]
```

```sh
shep create --type feature "add rate limiting to the api"
shep ps                     # watch it move through your pipelines
shep approve                # when it reaches your handoff step
```

Each task works in its own git worktree on its own branch, so several tasks can
run at once without touching each other's files.

## How it fits together

One shared SQLite database file connects everything: commands write to it, a
background supervisor reads it a few times a second and runs your steps, and
Herdr's notifications land in it so the supervisor knows when an agent has
finished. Open `shepherd-city.html` in a browser for an interactive map.

## Documentation

- [Configuration](docs/configuration.md) — pipelines, types, and validation
- [Step scripts](docs/step-scripts.md) — the contract between shepherd and your scripts
- [Commands](docs/commands.md) — the `shep` command reference
- [Architecture](docs/architecture.md) — the parts and how data flows between them

# shepherd — build plan

An agent orchestration system built as a Herdr plugin. One supervisor process, a
declarative pipeline engine, and step scripts in whatever language you like.

- **Engine:** Rust. One crate, `shepherd`; one binary, `shep`.
- **Policy:** TOML + scripts, living in the repo being worked on.
- **Substrate:** Herdr 0.8.0 (socket protocol 19). See `../herdr-findings.md`.
- **Design rationale:** `../herdr-components.html`, `../herdr-pubsub-sketch.html`,
  `../herdr-orchestrator-sketch.html`.

---

## 1. What this is

You ask an agent for something. It classifies the request and calls
`shep create --type feature "..."`. From there a state machine runs the work
through a sequence of pipelines — implement, review, hand it to you, integrate —
spawning Claude Code agents in Herdr panes where the work benefits from being
watchable, and running everything else as plain subprocesses.

Two sentences that determine most of the design:

1. **The engine decides nothing a human didn't write down.** Every branch comes
   from config.
2. **Nothing is asked of a worker agent.** Herdr reports when it stops, and lint,
   tests and review catch bad work without its cooperation.

### Explicit non-goals for v1

- No reconciliation loop. Completion is edge-triggered off Herdr events. A stuck
  task sits there until `shep retry`. Accepted because the failure is inert.
- No LLM in the engine. Classification happens in the agent you're already
  talking to; agent review happens in a script. The Rust binary has no API client.
- No pub/sub product. The event table is an audit trail, not a subscription bus.
- No IPC. The CLI and the supervisor are the same binary sharing one SQLite file;
  there is no socket, no protocol and no server. See §7.4.
- No multi-machine anything. One laptop, one Herdr session.

---

## 2. Vocabulary

| Term | Meaning |
| --- | --- |
| **task** | One unit of requested work. Has a brief, a type, and a position. |
| **type** | A named composition of pipelines. `feature`, `hotfix`. No loops. What the agent picks. |
| **pipeline** | A small state machine over steps. Owns its own loop and round cap. **Returns an outcome, which is why a pipeline can be used as a step.** |
| **step** | One script invocation. Resolves to `.shep/scripts/<step>.sh`. |
| **check** | A verdict plus evidence about a specific commit. Written by linters, test runs, reviewing agents and humans alike. |
| **event** | An append-only audit record. Nothing reads it to make a decision. |

Round is scoped to the innermost pipeline. "Round 2 of `review`" is meaningful;
"round 2 of `feature`" is not.

---

## 3. Technology

| Component | Crate / tool | Note |
| --- | --- | --- |
| Binary | one `shep` binary from the `shepherd` crate, `clap` subcommands | `shep supervise` is the daemon; everything else is a client. Shared types for free. |
| Concurrency | `std::thread` + `std::process` | **No async runtime.** With no socket to serve, the supervisor's whole job is: poll the database, spawn step scripts, block on `child.wait()`. One thread per in-flight step, and at three or four concurrent tasks that is cheaper and far simpler than `tokio`. Reach for async only if this ever grows a server. |
| Database | `rusqlite`, `bundled` feature | WAL mode, `busy_timeout` on every connection. This is the only IPC mechanism in the system. |
| Config | `serde` + `toml` | Deny unknown fields — a typo must be an error, not silence. |
| Subprocesses | `std::process::Command` | Step scripts, and `herdr` CLI calls. Capture stdout, take the last line, apply §7.1. |
| Herdr control | shell out to `herdr`, parse stdout | It already emits JSON. No need to speak its socket for commands. |
| Live view | none — `watch -n2 shep ps` | **No TUI.** A dashboard would be a third rendering of data you already have two views of: `shep ps` for detail and Herdr's own sidebar badges for the ambient glance. `watch` in a `[[panes]]` entry gets you a live pane with no dependency and no render loop. |
| Errors | `anyhow` in binaries, `thiserror` for library errors | |
| Logging | `tracing` + `tracing-subscriber` | Hook stdout is not a terminal; log to a file under the state dir. |

No HTTP client, no LLM SDK, no message broker, no socket, no TUI framework. The
heaviest dependency is SQLite, and it's vendored.

---

## 4. Layout

### The engine (this repo)

```
shepherd/
  Cargo.toml                 # package = "shepherd", [[bin]] name = "shep"
  herdr-plugin.toml          # [[startup]] [[events]] [[panes]] [[actions]]
  hooks/forward.sh           # 2 lines. appends the raw event and exits.
  src/
    main.rs                  # clap dispatch
    db/                      # schema, migrations, queries
    config/                  # pipeline + type parsing and validation
    engine/                  # the state machine: advance, resolve, park
    herdr/                   # thin wrappers over the herdr CLI
    cmd/                     # one module per subcommand
```

### Policy (in each target repo)

```
<repo>/.shep/
  config.toml
  scripts/
    code.sh  lint.sh  test.sh  agent_review.sh
    fix.sh   show_diff.sh  integrate.sh
```

**The filename is the registration.** `steps = ["lint"]` resolves to
`.shep/scripts/lint.sh`. There is no separate `handles = ...` declaration.
Fallback search path for project-agnostic scripts: `~/.config/shep/scripts/`.

Engine in the plugin, policy in the repo — `lint.sh` for a Rails app and a Python
library are not the same script, and both should be versioned with the code they
judge. Config is therefore loaded **per repo root**, not globally.

---

## 5. Config schema

```toml
# A PIPELINE is a state machine. It owns its loop and its cap.
[pipeline.implement]
steps = ["code"]
await = "agent_stopped"          # deferred — uses a pane

[pipeline.review]
steps        = ["lint", "test", "agent_review"]
on_fail      = "fix"             # a step inside THIS pipeline
max_rounds   = 3
on_exhausted = "reject"          # this pipeline's own outcome

[pipeline.handoff]
steps = ["show_diff"]
await = "human"                  # ends on `shep approve` / `shep reject`

# A TYPE is a composition. No loops here, so termination is obvious.
[type.feature]
description = "Normal change. Reviewed, then shown to you."
pipelines   = ["implement", "review", "handoff", "integrate"]

[type.hotfix]
description = "Urgent production fix. No review, no handoff."
pipelines   = ["implement", "integrate"]
```

`description` lives on **types only** — it exists so an agent can choose, and the
agent only ever chooses a type. It is what `shep types` prints and what an invalid
`--type` error lists.

`await` values: absent (synchronous), `agent_stopped`, `human`.

---

## 6. Data model

```sql
PRAGMA journal_mode = WAL;

CREATE TABLE task (
  id           TEXT PRIMARY KEY,
  brief        TEXT NOT NULL,          -- the brief lives here, not on disk
  type         TEXT NOT NULL,
  pipeline     TEXT,                   -- current pipeline within the type
  step         TEXT,                   -- current step within the pipeline
  round        INTEGER NOT NULL DEFAULT 0,
  status       TEXT NOT NULL,          -- queued running parked finished cancelled
  human_owned  INTEGER NOT NULL DEFAULT 0,  -- 1 = mute status events for its pane
  repo         TEXT NOT NULL,          -- which .shep/config.toml governs this
  worktree     TEXT, branch TEXT, base TEXT,
  workspace_id TEXT,
  created      INTEGER NOT NULL,
  updated      INTEGER NOT NULL
);

CREATE TABLE check_run (
  id         TEXT PRIMARY KEY,         -- c-7
  task_id    TEXT NOT NULL,
  pipeline   TEXT, step TEXT, round INTEGER,
  author     TEXT NOT NULL,            -- "agent_review" | "eslint" | "pytest" | "oguz"
  sha        TEXT NOT NULL,            -- what it judged. stale if head has moved.
  conclusion TEXT NOT NULL,            -- pass | fail
  body       TEXT,
  created    INTEGER NOT NULL
);

CREATE TABLE event (
  seq       INTEGER PRIMARY KEY AUTOINCREMENT,   -- the only ordering that matters
  ts        INTEGER NOT NULL,
  type      TEXT NOT NULL,             -- task.created task.step_finished task.parked ...
  task_id   TEXT,
  payload   TEXT,                      -- JSON
  caused_by INTEGER                    -- the seq that led here, for `shep trace`
);

CREATE TABLE pane_task (
  pane_id TEXT PRIMARY KEY,
  task_id TEXT NOT NULL
);

CREATE TABLE raw_event (               -- what Herdr said, as it said it
  seq  INTEGER PRIMARY KEY AUTOINCREMENT,
  ts   INTEGER NOT NULL,
  body TEXT NOT NULL
);
```

Notes that matter:

- **State and event commit in one transaction.** There is never an event for a
  change that didn't persist, or a change with no event.
- **Every state transition uses `BEGIN IMMEDIATE`.** The CLI and the supervisor are
  both writers, so `shep run review` firing while the supervisor is resolving a step
  is a real double-advancement race. Letting SQLite serialize the transactions is the
  fix — not optimistic versioning, not a lock file. Re-read the task row inside the
  transaction and bail if it moved.
- **Event names are fixed and few.** `task.step_finished {step, outcome, round}`,
  not `implement.finished` — otherwise editing config mints new protocol.
- **`pane_task` earns its keep twice:** it makes a Herdr event attributable, and it
  lets a bare `shep context` resolve its own task from `$HERDR_PANE_ID`.
- **`sha` on `check_run` is load-bearing.** A check is a verdict about a particular
  state of the code. `integrate` must refuse to pass on a check whose sha isn't
  head, or a stale pass waves a bad merge through.

---

## 7. Contracts

These are the interfaces that must not drift. Everything else is implementation.

### 7.1 Step script

**In — environment:**

```
SHEP_TASK_ID   SHEP_TYPE      SHEP_PIPELINE   SHEP_STEP   SHEP_ROUND
SHEP_WORKTREE  SHEP_BRANCH    SHEP_BASE       SHEP_REPO
SHEP_PANE      # present only if the task already has a bound pane
SHEP_DB        # so `shep` subcommands invoked by the script find the right store
```

Plus whatever Herdr injects. cwd is the worktree.

**Out — the last line of stdout must be one JSON object:**

```json
{"outcome": "pass", "note": "optional, human-readable"}
{"outcome": "reject"}
{"outcome": "started", "pane": "wA:p2"}
{"outcome": "error", "note": "why"}
```

Only the **last** line is the answer. Everything above it is captured as logs, so
a script is free to shell out to `pytest` or `claude -p` and let it print.

| outcome | Meaning |
| --- | --- |
| `pass` | Step succeeded. Advance. |
| `reject` | Step's verdict is negative. Take the pipeline's `on_fail`. |
| `started` | A promise, not an answer. Resolve later per the pipeline's `await`. |
| `error` | Something broke. Park the task. |

Non-zero exit, or an unparseable last line, is treated as `error` regardless of
what was printed.

### 7.2 Resolving a deferred step

- `await = "agent_stopped"` — on the pane's agent going `done` (or `pane.exited`),
  read the latest `check_run` for that task + pipeline + step + round. Its
  conclusion becomes the outcome. **No check means the step errored.**
- `await = "human"` — nothing resolves it but `shep approve` / `shep reject`, each of
  which is one transaction the supervisor picks up on its next poll. While waiting,
  `task.human_owned = 1`: status events for that pane are written to
  `raw_event` but advance nothing, and no deadline applies.

### 7.3 Check submission

```
shep check submit --pass|--fail [--author NAME]   # body on stdin
```

`shep` itself stamps `sha` (from `git rev-parse HEAD` in the worktree) plus
`task_id`, `pipeline`, `step` and `round` from the caller's environment, then writes
the row. **The submitter never supplies the sha** — otherwise a stale check becomes
an agent-behaviour bug instead of an impossible state.

### 7.4 There is no IPC

The CLI and the supervisor are the same binary over the same SQLite file. A command
is a transaction; the supervisor notices on its next poll. That is the entire
mechanism.

```
shep create --type feature "..."     # INSERT task + event, status=queued
shep bind-pane wA:p2                 # INSERT pane_task
shep check submit --pass             # INSERT check_run, sha stamped here
shep approve | reject                # resolve an await="human" pipeline
shep run <pipeline>                  # set the task's pipeline, out of band
shep cancel | retry                  # UPDATE status; cancel also shells out to herdr
shep pause | resume                  # a flag row the supervisor reads each tick
shep context | read | ps | get | trace | types | status     # pure reads
```

Rules that replace what a server would have enforced:

- **WAL, plus `busy_timeout` on every connection.** Concurrent writers wait rather
  than fail.
- **`BEGIN IMMEDIATE` for any state transition**, so writers serialize (§6).
- **Keep transactions short.** Never hold one open across a subprocess.
- **Writes go through one function per transition**, shared by the CLI and the
  supervisor. Consistency comes from shared code, not from a transport.

Consequences worth knowing: every read command works with the supervisor down, and
errors surface directly from the command rather than round-tripping. Task creation
picks up on the next poll instead of instantly, which is ~200ms and does not matter.

**What would bring IPC back:** wanting the supervisor somewhere the CLI isn't, or
wanting to push to long-lived subscribers instead of having them poll.

### 7.5 Agent-facing surface

An agent in a pane knows exactly three things:

```
shep context                    # my brief. resolves the task from $HERDR_PANE_ID
shep read c-7                   # one addressed artefact
shep check submit --pass|--fail  # only for steps whose deliverable IS a verdict
```

Nothing else, and all three are local commands against a database file — there is
nothing for an agent to connect to and nothing to be down. The opening prompt a step
script sends is one line pointing at `shep context`, and the wording after that is
what makes a step a review rather than an implementation — so **the script owns the
prompt, not the engine.**

---

## 8. Validation

At config load, and via `shep validate` as an explicit command:

- every step resolves to an existing executable file, or to another pipeline
- every pipeline named by a type exists
- `on_fail` targets a step **inside the same pipeline** — round is scoped there, so
  a cross-pipeline target is meaningless
- `on_fail` set without `max_rounds` is rejected: that's an unbounded loop
- `await` is absent, `agent_stopped`, or `human`
- no cycles in pipeline composition; nesting depth capped at 2
- no `await = "human"` pipeline inside a loop, or the loop asks you N times
- unknown TOML keys are an error, not a warning

**Do not exit on invalid config.** A `[[startup]]` failure does not stop the Herdr
server, so exiting means the supervisor silently isn't running. Start anyway,
refuse to accept tasks, and report through `shep status` plus a Herdr
notification. Dying is the one way to fail quietly here.

Also verify the plugin registered what you wrote: `herdr plugin list --json`.
Herdr silently ignores unknown manifest fields and only *warns* on unknown event
names, so a typo in either fails silently.

---

## 9. Milestones

Each one ends with something you can run.

### M1 — Skeleton and store
Binary with `clap`. SQLite schema + migrations, WAL, `busy_timeout`. A
`transition()` helper that wraps `BEGIN IMMEDIATE`, re-reads the task, writes state
and event together, and bails if the row moved. `shep supervise` starts and polls;
`shep status` reports whether it's alive (a heartbeat row will do).
**Done when:** two concurrent writers hammering `transition()` never lose an update
or return `SQLITE_BUSY`, and `shep status` is correct with the supervisor up and
down.

### M2 — Herdr edge
`herdr-plugin.toml` with `[[startup]]` and `[[events]]` for
`pane.agent_status_changed`, `pane.exited`, `pane.closed`. `hooks/forward.sh`
appends to `raw_event`.
**Done when:** `herdr plugin list --json` shows all three events with no
`unknown event` warnings, and starting an agent by hand lands rows in `raw_event`.

### M3 — Config
Parse and validate pipelines and types. `shep types`, `shep validate`. Invalid
`--type` returns the menu.
**Done when:** every rule in §8 has a test, and a broken config yields a usable
error rather than a panic.

### M4 — One synchronous pipeline
`task.create` → run a type whose only pipeline is `[lint]` → `pass` → finished.
Step spawning, the stdout contract, `task.step_finished`, park on error.
**Done when:** a lint failure parks the task and `shep trace` shows why.

### M5 — Deferred, with a pane
`implement`: worktree create, pane split, `shep bind-pane`, `agent start`,
`agent prompt`, exit `started`. Resolution off the status event via `check_run`.
`shep context` resolving from `$HERDR_PANE_ID`.
**Done when:** an agent implements something unattended and the task advances on
its own.

### M6 — The review pipeline
`lint`, `test`, `agent_review` (headless `claude -p`), `check submit` with sha
stamping, `on_fail` → `fix` → loop, `max_rounds`, `on_exhausted`.
**Done when:** a deliberately bad implementation loops twice and then exhausts.

### M7 — Handoff
`await = "human"`, `human_owned` muting, `show_diff.sh` splitting **and focusing**
a pane, a Herdr notification, `shep approve` / `shep reject`, `shep run <pipeline>`
out of band.
**Done when:** you can converse with the agent through a whole handoff without the
state machine moving, then re-run review by hand, then approve.

### M8 — Integrate and teardown
Rebase, merge, stale-check refusal, `worktree remove`, workspace close.
**Done when:** a `feature` task goes end to end with one rejection and one handoff.

### M9 — Visibility
`shep ps` as a plain aligned table, `shep trace` as a tree. Sidebar badges via
`workspace report-metadata` (custom tokens must start with `$`) — highest value per
line in the whole system, since it turns Herdr's existing sidebar into the status UI.
A `[[panes]]` entry running `watch -n2 shep ps` if you want a live pane.
`[[actions]]` bound to keys for cancel and pause.
**Done when:** you can tell what every task is doing without typing anything, from
the sidebar alone.

**M7 is where this stops being a batch runner and becomes something you'd use.**
Consider stopping after it and living with the result for a fortnight before
building M9.

---

## 10. Deferred, with the trigger for revisiting

| Deferred | Build it when |
| --- | --- |
| Reconciliation loop | Tasks get stuck often enough that `shep retry` is annoying. Add `shep sync` as a verb first — a manual reconcile is most of the value. |
| Per-step deadlines | A hung `claude -p` or an undetected agent wedges something overnight. One timestamp comparison, exempt human steps. |
| Observers / durable subscriptions | You genuinely want something to happen on completion that isn't a pipeline step. Cursor per subscriber, idempotent scripts. |
| A socket / IPC layer | You want the supervisor somewhere the CLI isn't, or push instead of poll. Until then the shared database is the interface. |
| A real TUI | `watch shep ps` plus sidebar badges stop being enough — most likely because you want to act on a selected task rather than just read. `[[actions]]` keybindings cover most of that first. |
| Concurrency cap and scheduling | You run more than two or three tasks at once. |
| Conflict-resolver tasks | An auto-merge conflicts and you'd rather not do it by hand. |
| DAG dependencies between tasks | A single request needs several tasks in order. |

---

## 11. Known risks

- **Completion depends on one heuristic.** Herdr's agent-status detection is the
  only trigger for deferred steps. Hooking `pane.exited` and `pane.closed`
  alongside `agent_status_changed` covers the common non-`done` endings, but an
  agent Herdr classifies as `unknown` will hang. The failure is inert.
- **Synchronous work dies with the supervisor.** A step in `running` with no bound
  pane was synchronous and got orphaned — on startup, re-run it. With a bound pane,
  leave it alone.
- **Alternate-screen output is unrecoverable.** Don't build anything that depends
  on scraping an agent's pane. Checks in the database are the record.
- **Herdr silently accepts nonsense in the manifest.** Unknown fields are ignored
  and unknown event names only warn. Assert on `herdr plugin list --json` in CI.
- **`herdr agent start` needs a pane already at an interactive prompt** and never
  creates layout. Split first, always.
- **Autonomy flags go after `--`** or an unattended agent parks in `blocked` on its
  first approval prompt.

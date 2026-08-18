# Architecture

Shepherd is one Rust program, `shep`, run two ways: as the commands you type,
and as a background supervisor that Herdr starts. Everything between them goes
through one shared SQLite database file.

For an interactive map of the system, open `shepherd-city.html` (in the
repository root) in a browser.

## The parts

| Part | What it does |
| --- | --- |
| `shep` (CLI) | The program you and agents type commands into. Each command reads or writes the shared database file directly. |
| `shep supervise` | A background process with a simple loop: check the database for work, run the right step script, wait for it to finish, record the result. Each running step gets its own thread. |
| `engine/` | The rules for moving a task forward: which step comes next, when to retry, when to stop retrying, and when to put a task on hold. |
| SQLite database | One file on disk that every part reads and writes. Writers take turns; each state change and its log entry are saved together. |
| Herdr | The terminal application shepherd runs inside. It manages panes, starts and watches coding agents, and reports when an agent stops. |
| `hooks/forward.sh` | A two-line script. When Herdr reports that something happened, it writes the notification into the database, word for word, and exits. |
| `.shep/config.toml` | Per-repository configuration defining the pipelines and task types for that project. |
| `.shep/scripts/` | The scripts that do the actual work. The step's name is the script's filename. |
| git worktrees | Each task gets its own working copy of the repository, on its own branch, so several tasks proceed at once. |

## How data flows

1. **A command writes.** `shep create` saves a new task to the database, in one
   transaction, together with a log entry.
2. **The supervisor reads.** A few times a second it checks the database. When
   it finds work, it looks up the task's type in the project's configuration and
   runs the next step's script, with the task's details in the environment.
3. **The script reports.** Its final line of output is one JSON object: `pass`,
   `reject`, `started`, or `error`. The supervisor records the result and the
   engine decides the next move.
4. **Herdr reports.** When an agent stops, a pane closes, or a workspace closes,
   Herdr runs `hooks/forward.sh`, which writes the notification into the
   database. The supervisor reads those records, works out what changed, and
   resolves any step that was waiting.
5. **You decide.** A pipeline with `await = "human"` sits until `shep approve`
   or `shep reject`, each of which is one database write the supervisor picks up
   on its next check.

## The database

| Table | Holds |
| --- | --- |
| `task` | Each task's description, type, current position and status. |
| `check_run` | Pass/fail verdicts, each tied to the exact commit it judged. |
| `event` | A permanent log of everything that happened (shown by `shep trace`). |
| `pane_task` | Which terminal pane belongs to which task. |
| `raw_event` | Herdr's notifications, stored word for word. |

Two rules keep concurrent writers safe: every state transition happens inside a
single immediate transaction, so writers take turns; and a state change and its
log entry are always saved together, so the log always matches reality. Each
transition re-reads the task inside the transaction and stops if another writer
moved it first.

## The Herdr edge

Shepherd registers with Herdr through `herdr-plugin.toml`:

- a startup entry that launches the supervisor when Herdr starts
- event hooks for `pane.agent_status_changed`, `pane.exited`, `pane.closed`
  and `workspace.closed`, all pointing at `hooks/forward.sh`

Commands travel the other way through the `herdr` CLI: split a pane, start an
agent, send a prompt, report sidebar status. Herdr's sidebar badges show each
task's status at a glance.

## Recovery

- A task whose step broke goes on hold; `shep retry` runs it again.
- If the supervisor dies mid-step, steps that were running as plain scripts are
  run again on startup; steps waiting on an agent are left alone, and resolve
  from Herdr's notifications as usual.
- If the configuration is broken at startup, the supervisor stays running,
  declines new work, and reports the problem through `shep status`.

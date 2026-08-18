# Notes from building against Herdr

Additions to `../herdr-findings.md`, verified against herdr 0.8.0 (protocol 19)
while building. Each one changed a decision, so each one is here rather than in a
commit message.

## Workspace close is invisible to the pane hooks (2026-08-18)

Closing a workspace fires **neither `pane.closed` nor `pane.exited`** for the
panes it takes with it. Probed four times — `herdr workspace close` on
single-pane and two-pane workspaces — and the only hook that fired was
`workspace.closed`.

This matters because a task gets a workspace of its own, so closing that
workspace is an ordinary way for its agent pane to vanish. With only the three
hooks PLAN §M2 lists, that ending is invisible and the task hangs. The manifest
therefore also hooks `workspace.closed`.

Its payload carries `data.workspace_id` and no pane id:

```json
{"event":"workspace_closed",
 "data":{"type":"workspace_closed","workspace_id":"wT","workspace":{...}}}
```

So resolving it means going from workspace to task, which is what
`task.workspace_id` is for.

## `pane.exited` lags (2026-08-18)

An `exit` in a pane produced `pane.exited` roughly 20–25 seconds later, not
promptly. Once observed the pane was already gone from `herdr pane list`. And a
pane whose workspace was closed within a couple of seconds of the exit never
produced the event at all — the teardown appears to overtake it.

Consequence: `pane.exited` is a backstop, not a trigger to design around.
`pane.agent_status_changed` remains the primary signal.

## `pane report-agent` synthesises status changes (2026-08-18)

```
herdr pane report-agent <pane_id> --source shep --agent claude --state working
```

fires a real `pane.agent_status_changed` hook without running an agent, which
makes the deferred-step path (PLAN §M5) testable in about a second. Reporting
`idle` on an unfocused pane surfaced as `done`, exactly as
herdr-findings §5.1 describes.

## A freshly split pane is not an available shell (2026-08-18)

`herdr agent start` on a pane split a moment earlier fails:

```json
{"error":{"code":"agent_pane_busy",
          "message":"agent target pane wW:p2 is not an available shell"}}
```

herdr-findings §5.3 says an available pane must be "at its interactive prompt with
no foreground command"; what it does not say is that a new pane takes a second or
two to get there, and there is no readiness field on `pane get` — `agent_status`
is `unknown` either way.

`herdr pane process-info --pane <id>` is where the answer is, but not in the field
you would reach for first. `foreground_process_group_id` equals `shell_pid` from
the moment the pane exists, because the shell's own rc files run as its children in
its own process group — so comparing those two says "ready" while zsh is still
sourcing atuin. The signal that works is the foreground process *list*: exactly one
entry, and its pid the shell's.

Probed on a fresh split, polling once a second:

```
t+1s  procs=[bash bash zsh]                     agent start → agent_pane_busy
t+2s  procs=[printf sed zsh zsh tail zsh ...]   agent start → agent_pane_busy
t+3s  procs=[atuin zsh]                         agent start → agent_pane_busy
t+4s  procs=[zsh]                               agent start → accepted
```

So roughly 3-4 seconds of shell startup on this machine, and `code.sh` polls for
that one-entry state before starting an agent. (Note the flag, too: `herdr pane
process-info <pane_id>` positionally is "unknown option".)

## `agent start` produces a status event of its own (2026-08-18)

`agent start` returns once Herdr considers the agent ready for input, and that
readiness is itself a `pane.agent_status_changed` — observed as `idle` for a pane
split with `--no-focus`, arriving before the prompt was sent.

So "the agent finished" cannot be read off `done`/`idle` alone: the first thing
every deferred step would see is its own agent starting up. Resolution needs a
remembered `working` first, which is what `pane_agent` is for.

## Getting environment into an agent's pane (2026-08-18)

`herdr worktree create` takes no `--env` (only `workspace create` does), and its
response is a `WorkspaceInfo` — no worktree path, no root pane id. So `code.sh`:

- passes `--path` itself, since that is the only way to know where the worktree is;
- reads the root pane from `herdr pane list --workspace <id>`
  (`.result.panes[0].pane_id`);
- and splits *that* with `--env SHEP_DB=... --env SHEP_TASK_ID=...`, which is what
  makes a bare `shep context` work in the agent's pane.

The split is not just for the environment: it leaves the worktree's own shell pane
next to the agent, which is where you end up standing when you go and look.

## `agent prompt --wait` settles for the state it started in (2026-08-18)

herdr-findings §5.3 warns that `--wait` "tracks lifecycle state, not an individual
turn: if the agent is already working, completion of the *active* turn can satisfy
it". The idle case is worse than that warning suggests:

```
$ herdr agent prompt w0:p2 "Reply with the word ack." --wait --until idle --until done --timeout 1200000
{"result":{"agent":{"agent_status":"done", ...}}}     # instantly
```

An idle agent already satisfies `--until idle`, so the call returns before the agent
has read anything. A repair step built on it would report success while the repair
was still being typed.

So `prompt_and_wait` in `.shep/scripts/lib.sh` is three calls, not one: `agent
prompt`, then poll `agent get` until the status has left `idle` (the prompt landed),
then `agent wait --until idle --until done`. Only the middle step makes the third
one mean anything.

## Not a Herdr finding: macOS bash and apostrophes (2026-08-18)

Worth recording because it produced a step that failed with an empty error message.
macOS ships bash 3.2, which mis-parses an apostrophe inside `${var:-default}` in a
here-document — the quote swallows the terminator, `read` assigns nothing, and the
prompt is an unbound variable:

```bash
f=""
read -r -d '' p <<PROMPT
X ${f:-the repo's rules} Y
PROMPT
echo "[${p:-UNSET}]"     # bash 3.2: [UNSET].  bash 5: [X the repo's rules Y]
```

Step scripts are policy in a repo, so they run under whatever `bash` that machine
has. Every prompt default is therefore built before its heredoc, and the rule is
written at the top of `.shep/scripts/lib.sh` where the next person will meet it.

## An agent name is a task's name, so a retry collides with itself (2026-08-18)

`herdr agent start` refuses a name that is already in use by a live agent
(herdr-findings §5.3):

```json
{"error":{"code":"agent_name_taken",
          "message":"agent name shep-t-1 is already used; candidates: pane_id=w12:p2 ..."}}
```

Since `code.sh` names the agent after the task, that fires on exactly the retry
you would most want to work: the first attempt started an agent and then died
somewhere after, so the name is taken by an agent that *is* this task's.

Working around the name would be the wrong fix. `code.sh` adopts instead — it asks
`herdr agent get shep-<task>` for a pane and takes it, binding that pane to the
task. And adoption comes *before* the pane the store remembers, because a failed
attempt can leave a bound pane it never started anything in: a live agent named
for the task is the better authority on where that task's agent is.

## The hook writes to the default store, so `--db` is a trap (2026-08-18)

`hooks/forward.sh` runs `shep forward` with whatever environment the Herdr server
has, which does not include `$SHEP_DB`. So every Herdr event lands in the default
store (`~/.local/state/shep/shep.db`) regardless of which store the supervisor you
started is driving.

Run a supervisor with `--db /somewhere/else` and every deferred step hangs: the
work happens, the agent stops, and the edge is filed in a store nobody is reading.
Cost half an hour of a live M7 run before it was obvious.

Two things follow. Tests use `--db` and are unaffected, because they write
`raw_event` rows themselves. But anything live must use the default store — and
`shep status` ought to say when the store it is reporting on is not the one the
hooks write to (M9's business).

The other half of the same trap: a pane's environment is fixed when it is split, so
`$SHEP_DB` in an agent's pane points at whatever store existed then. Move the store
and that agent's `shep check submit` writes nowhere useful.

## Hook stdout is visible and worth using

`herdr plugin log list --plugin shepherd` shows each hook run with its exit code,
stdout and stderr. `shep forward` prints one line naming the event and the
`raw_event` seq it wrote, which turns that log into a usable trace of the edge.

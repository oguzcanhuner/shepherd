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

## Hook stdout is visible and worth using

`herdr plugin log list --plugin shepherd` shows each hook run with its exit code,
stdout and stderr. `shep forward` prints one line naming the event and the
`raw_event` seq it wrote, which turns that log into a usable trace of the edge.

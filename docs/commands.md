# Commands

`shep` is one program. Every command reads or writes the shared database file
directly; the background supervisor sees new writes the next time it checks,
within a fraction of a second. Read-only commands work whether or not the
supervisor is running.

## Creating and steering tasks

```
shep create --type <type> "<brief>"   # create a task; it starts on the next poll
shep approve                          # settle a task waiting for your decision
shep reject                           #   ... negatively
shep cancel <task>                    # stop a task (also closes its pane)
shep retry <task>                     # re-run a task that is on hold
shep pause | shep resume              # stop and restart the supervisor's intake
shep run <pipeline>                   # point a task at a pipeline by hand
```

## Reading

```
shep ps                # list tasks and where each one is
shep get <task>        # one task in full
shep trace <task>      # the task's history, as a tree of events
shep context           # the current task's brief (resolves from the pane)
shep read <id>         # one addressed item, such as a check c-7
shep types             # the task types an agent can pick, with descriptions
shep status            # supervisor health and configuration state
shep validate          # check the configuration and report problems
shep skill             # print the orchestrator skill (see below)
```

`shep context` works without arguments inside a task's pane: the pane is linked
to the task when the pane is created, and shep looks the task up from the
`$HERDR_PANE_ID` that Herdr provides.

## Recording verdicts

```
shep check submit --pass|--fail [--author NAME]   # body on stdin
```

Records a pass/fail verdict tied to the exact commit in the working copy. The
task, pipeline, step and round come from the caller's environment; the commit
is read and stamped by shep itself.

## The orchestrator skill

```
shep skill
```

Prints, as markdown, everything a conversational agent needs to act as an
orchestrator: when a request should become a task, how to write a good brief,
how to watch tasks, and how to relay approvals and rejections. The repository's
task types are read live from its config, so the menu in the output is always
current. Load it into an agent's context — as a skill file, a section of a
`CLAUDE.md`, or pasted into the conversation:

```sh
shep skill > .claude/skills/shepherd-orchestrator/SKILL.md
```

## The supervisor

```
shep supervise         # the background process; Herdr starts it on launch
```

Its loop: check the database for work, run the right step script, wait for it
to finish, record the result. It also reads Herdr's stored notifications to
notice when an agent has stopped. A live view is one pane away:

```
watch -n2 shep ps
```

# Step scripts

Steps are the scripts that do the actual work — lint, test, write code, review,
fix, show a diff, merge. They live in `.shep/scripts/` in the repository, and a
step named `lint` runs the file `.shep/scripts/lint.sh`. Putting a file there is
all it takes to add a step. Scripts can be written in any language.

## What a script receives

Each script runs with the task's working copy as its current directory and
these environment variables set:

```
SHEP_TASK_ID   SHEP_TYPE      SHEP_PIPELINE   SHEP_STEP   SHEP_ROUND
SHEP_WORKTREE  SHEP_BRANCH    SHEP_BASE       SHEP_REPO
SHEP_PANE      # present when the task already has a bound terminal pane
SHEP_DB        # so `shep` commands run by the script find the right database
```

## What a script reports

The final line of the script's output must be one JSON object:

```json
{"outcome": "pass", "note": "optional, human-readable"}
{"outcome": "reject"}
{"outcome": "started", "pane": "wA:p2"}
{"outcome": "error", "note": "why"}
```

| outcome | Meaning |
| --- | --- |
| `pass` | The step succeeded. The task moves forward. |
| `reject` | The step's verdict is negative. The pipeline runs its retry step. |
| `started` | The script launched an agent that will finish later. The pipeline's `await` says how the result arrives. |
| `error` | Something broke. The task goes on hold until `shep retry`. |

Everything printed before the final line is kept as a log, so a script is free
to run `pytest` or `claude -p` and let it print. A non-zero exit code, or a
final line that fails to parse, is treated as an error.

## Steps that launch an agent

For work worth watching, a script opens a Herdr pane, links it to the task,
starts a coding agent, and reports `started`:

```sh
pane=$(herdr pane split --json | jq -r .pane_id)
shep bind-pane "$pane"
herdr agent start "$pane" -- claude --permission-mode acceptEdits
herdr agent prompt "$pane" "Run \`shep context\` for your brief, then implement it."
echo '{"outcome":"started","pane":"'"$pane"'"}'
```

The script owns the prompt. The wording after "run `shep context`" is what makes
one step an implementation and another a review.

An agent working in a pane uses three commands:

```
shep context                       # read my brief
shep read c-7                      # fetch one referenced item
shep check submit --pass|--fail    # record a verdict (body on stdin)
```

All three are local commands against the shared database file.

## Checks

`shep check submit` records a pass/fail verdict together with the exact commit
it judged. Shep reads the commit from the working copy itself and stamps it on
the record, along with the task, pipeline, step and round from the caller's
environment. The integrate step confirms, before merging, that the recorded
pass verdicts were given for the exact commit being merged.

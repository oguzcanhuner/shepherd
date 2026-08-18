#!/usr/bin/env bash
# handoff: put the diff in front of you, and stop.
#
# The one step that ends by asking for a person. It splits a pane, focuses it —
# unlike everything else here, which works with --no-focus — shows the diff, and
# notifies. Then it returns `started`, and nothing but `shep approve` or
# `shep reject` will move the task (PLAN §7.2).
set -uo pipefail
lib="$(dirname -- "$0")/lib.sh"
# shellcheck source=lib.sh
. "$lib" || { printf '{"outcome":"error","note":"cannot source %s"}\n' "$lib"; exit 0; }

command -v herdr >/dev/null || die "no herdr on PATH"

base=${SHEP_BASE:-main}
worktree=${SHEP_WORKTREE:-$SHEP_REPO}
# The task's own workspace first: the diff belongs next to the work, not next to
# whoever started the supervisor. $HERDR_WORKSPACE_ID is the fallback, for a task
# that never got a workspace of its own — and it is only ever right by accident,
# since it is the supervisor's workspace rather than the task's.
workspace=$("$shep" get "$SHEP_TASK_ID" --json | jq -r '.workspace_id // empty')
workspace=${workspace:-${HERDR_WORKSPACE_ID:-}}

# Split from a pane in that workspace, so the diff lands beside the agent rather
# than wherever the supervisor happens to be.
if [ -n "$workspace" ]; then
  anchor=$(herdr pane list --workspace "$workspace" | jq -r '.result.panes[0].pane_id // empty')
else
  anchor=${SHEP_PANE:-}
fi
[ -n "$anchor" ] || die "nowhere to put the diff: this task has no workspace and no pane"

split=$(herdr pane split "$anchor" --direction down --cwd "$worktree" --focus \
          --env "SHEP_DB=$SHEP_DB" --env "SHEP_TASK_ID=$SHEP_TASK_ID" --env "SHEP_BIN=$shep" 2>&1) \
  || die "herdr pane split failed: $(oneline "$split")"
pane=$(printf '%s' "$split" | jq -r '.result.pane.pane_id // empty')
[ -n "$pane" ] || die "herdr did not say which pane it split"

# Bind it too, so the pane you read the diff in is a pane where `shep approve`
# needs no arguments.
"$shep" bind-pane "$pane" >/dev/null || die "could not bind pane $pane"

wait_for_shell "$pane" || echo "pane $pane is slow to come up; sending the diff anyway"
range="$base...HEAD"
herdr pane run "$pane" "git -C '$worktree' log --oneline '$base..HEAD'; git -C '$worktree' diff --stat '$range'; git -C '$worktree' diff '$range'" \
  >/dev/null 2>&1 || echo "could not run git in $pane; the pane is there to do it in"

# A handoff that does not interrupt you is a handoff you find tomorrow.
herdr notification show "$SHEP_TASK_ID is ready for you" \
  --body "$(printf '%s' "${SHEP_BRANCH:-HEAD}") — read the diff, then shep approve or shep reject" \
  --sound request >/dev/null 2>&1 || true

# One last line, and it carries the pane: that is what the awaiting event records.
printf '{"outcome":"started","pane":"%s","note":%s}\n' \
  "$pane" "$(jsonstr "the diff is in $pane; nothing moves until you approve or reject")"

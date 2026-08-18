#!/usr/bin/env bash
# fix: the repair step this pipeline's rejections go to (on_fail).
#
# It talks to the agent already sitting in this task's pane — the one that wrote the
# code and still has the whole context of writing it. Blocking, so this stays an
# ordinary synchronous step and `review` needs no await.
#
# A task with no pane (a repo-level review with no `implement` before it) gets a
# headless `claude -p` instead: same prompt, no context, no pane.
#
# Whether the repair worked is never this script's opinion. The pipeline runs again
# from the top, and lint, tests and review say.
set -uo pipefail
lib="$(dirname -- "$0")/lib.sh"
# shellcheck source=lib.sh
. "$lib" || { printf '{"outcome":"error","note":"cannot source %s"}\n' "$lib"; exit 0; }

# A repair is not a quick call, and a step that hangs for ever is worse than one
# that gives up: 20 minutes, then say so.
timeout_ms=${SHEP_FIX_TIMEOUT_MS:-1200000}

# What went wrong is on the record: the failing checks from the round that just
# ended, which is this round minus one.
failed_round=$((${SHEP_ROUND:-1} - 1))
failures=$("$shep" get "$SHEP_TASK_ID" --json | jq -r --argjson round "$failed_round" '
  [.checks[] | select(.conclusion == "fail" and .round == $round)]
  | if length == 0 then empty
    else map("### \(.author) (\(.step // "?")) said:\n\n\(.body // "no detail")") | join("\n\n")
    end')
# Built before the heredoc, never inside it: bash 3.2 and apostrophes (lib.sh).
if [ -z "$failures" ]; then
  failures="No failing check left a body, so run this repo's own lint and tests to find out what is wrong."
fi
branch=${SHEP_BRANCH:-HEAD}

read -r -d '' prompt <<PROMPT
Round $failed_round of review rejected this work. Fix it here on branch $branch, and commit.

$failures

Fix the causes, not the symptoms: do not weaken a test, delete an assertion or silence a lint to make something quiet. If a check is wrong about the code, leave the code alone and say so in the commit message. Run \`$shep context\` if you need the brief again.

Lint, tests and review all run again after you stop, so you do not need to tell anyone how it went.
PROMPT

before=$(git rev-parse HEAD 2>/dev/null || echo none)

if [ -n "${SHEP_PANE:-}" ] && command -v herdr >/dev/null &&
   herdr agent get "$SHEP_PANE" >/dev/null 2>&1; then
  echo "repairing through the agent already in $SHEP_PANE"
  problem=$(prompt_and_wait "$SHEP_PANE" "$prompt" "$timeout_ms") || die "$problem"
  how="the agent in $SHEP_PANE"
else
  command -v claude >/dev/null || die "no agent pane for this task and no claude on PATH"
  echo "no agent pane for this task; repairing headless"
  out=$(claude -p "$prompt" --permission-mode acceptEdits 2>&1) \
    || die "claude -p failed: $(oneline "$(printf '%s' "$out" | tail -c 400)")"
  printf '%s\n' "$out"
  how="a headless agent"
fi

after=$(git rev-parse HEAD 2>/dev/null || echo none)
if [ "$before" = "$after" ]; then
  # Not an error: the next round will reject the same way and spend a round, which
  # is exactly the bound `max_rounds` is there to provide.
  say pass "$how left HEAD where it was, so round $((failed_round + 1)) judges the same commit"
else
  say pass "$how committed $after in answer to round $failed_round"
fi

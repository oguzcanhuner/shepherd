#!/usr/bin/env bash
# agent_review: a strict reading of the diff by an agent with no stake in it.
#
# Headless, so it is an ordinary synchronous step: no pane, no await, and the
# verdict is this script's last line. The check it writes is the durable record —
# `integrate` will refuse to merge on a check whose sha is not head (PLAN §6).
set -uo pipefail
lib="$(dirname -- "$0")/lib.sh"
# shellcheck source=lib.sh
. "$lib" || { printf '{"outcome":"error","note":"cannot source %s"}\n' "$lib"; exit 0; }

command -v claude >/dev/null || die "no claude on PATH"

base=${SHEP_BASE:-main}
range=$(git merge-base "$base" HEAD 2>/dev/null) || die "no merge base with $base"
diff=$(git diff "$range"..HEAD)
# The prompt goes in argv, so a runaway diff would fail the exec rather than the
# review. Cut it, and say so, rather than pretending it was all read.
limit=${SHEP_REVIEW_DIFF_BYTES:-200000}
if [ "$(printf '%s' "$diff" | wc -c)" -gt "$limit" ]; then
  diff="$(printf '%s' "$diff" | head -c "$limit")

[diff truncated at $limit bytes; read the rest from the repo yourself]"
fi
if [ -z "$diff" ]; then
  # Nothing to judge is not a failure, and it is not a pass to record either.
  say pass "no changes against $base to review"
  exit 0
fi

brief=$("$shep" context 2>/dev/null || echo "(no brief available)")
# Built before the heredoc, never inside it: bash 3.2 and apostrophes (lib.sh).
branch=${SHEP_BRANCH:-HEAD}

# The script owns the prompt, and this wording is the whole difference between a
# review step and an implementation one (PLAN §7.5). The last line is the verdict,
# for the same reason a step's last line is: everything above it is reasoning.
read -r -d '' prompt <<PROMPT
You are reviewing someone else's work on branch $branch, against $base. You did not write it and you are not being asked to fix it.

$brief

Read the diff below, and whatever else in the repo you need in order to judge it. Be strict about correctness, about the change matching the brief, and about anything that would embarrass the author in review. Ignore taste you cannot justify.

Then write a short summary — what the change does, and every problem you found, worst first. End your reply with exactly one of these lines, and nothing after it:

VERDICT: pass
VERDICT: fail

Fail it if the brief has not been met, or if you found a problem that should block the merge. The diff:

$diff
PROMPT

# Read-only tools: a reviewer that edits the code is no longer reviewing it.
review=$(claude -p "$prompt" \
  --allowed-tools Read Grep Glob \
  --permission-mode plan 2>&1) || die "claude -p failed: $(printf '%s' "$review" | tail -c 300)"

printf '%s\n' "$review"
verdict=$(printf '%s' "$review" | grep -oE '^VERDICT: (pass|fail)' | tail -1 | awk '{print $2}')

case "$verdict" in
  pass)
    printf '%s' "$review" | "$shep" check submit --pass --author agent_review >/dev/null \
      || die "could not record the review"
    say pass "the reviewer passed it"
    ;;
  fail)
    printf '%s' "$review" | "$shep" check submit --fail --author agent_review >/dev/null \
      || die "could not record the review"
    say reject "the reviewer found something"
    ;;
  *)
    # No verdict line means the review did not happen, whatever else was printed.
    # Calling that a pass would make a broken reviewer indistinguishable from a
    # clean bill of health.
    die "the reviewer ended with no VERDICT line"
    ;;
esac

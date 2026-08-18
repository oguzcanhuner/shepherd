#!/usr/bin/env bash
# implement: put an agent in a pane, in a worktree of its own, and let it work.
#
# The order here is forced by Herdr (herdr-findings §5.3, §6): `worktree create`
# returns a workspace, `agent start` never creates layout, and a pane must be at
# an interactive prompt before an agent can be started in it. So: worktree, split,
# bind, start, prompt — and then get out of the way. This step returns `started`,
# which is a promise; what redeems it is the agent going `done`, and the answer
# comes from the check it leaves behind (PLAN §7.2).
#
# Every stage is skippable, because a retry runs this again: a task that already
# has a pane must not get a second worktree.
set -uo pipefail

shep=${SHEP_BIN:-shep}
die() { printf '{"outcome":"error","note":%s}\n' "$(jsonstr "$1")"; exit 0; }
jsonstr() { printf '%s' "$1" | jq -Rs .; }
oneline() { printf '%s' "$1" | tr '\n' ' ' | cut -c1-300; }

command -v jq >/dev/null || { printf '{"outcome":"error","note":"no jq on PATH"}\n'; exit 0; }
command -v herdr >/dev/null || die "no herdr on PATH"

branch=${SHEP_BRANCH:-shep/$SHEP_TASK_ID}
base=${SHEP_BASE:-$(git -C "$SHEP_REPO" rev-parse --abbrev-ref HEAD)}
agent_kind=${SHEP_AGENT_KIND:-claude}
# Autonomy flags go after `--`, or an unattended agent parks in `blocked` on its
# first approval prompt (PLAN §11). It has to be able to run one command at the
# end — `shep check submit` is how the step gets its answer — so a mode that asks
# before running commands would deadlock the step by design. Narrow it per repo if
# you would rather: SHEP_AGENT_FLAGS='--permission-mode acceptEdits'.
agent_flags=${SHEP_AGENT_FLAGS:---dangerously-skip-permissions}

# Herdr reports an agent's status but says nothing about whether it can be
# prompted, so both waits below are the difference between an unattended step that
# works and one that silently never starts.

# A freshly split pane is not an available shell straight away, and `agent start`
# refuses a busy one with `agent_pane_busy`. Ready means the foreground process
# group is the shell itself: nothing is running in front of the prompt.
wait_for_shell() {
  deadline=$((SECONDS + 20))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if info=$(herdr pane process-info --pane "$1" 2>/dev/null) && [ "$(printf '%s' "$info" | jq -r '
          .result.process_info
          | (.shell_pid != null)
            and ((.shell_pid | tostring) == (.foreground_process_group_id | tostring))')" = true ]; then
      return 0
    fi
    sleep 0.2
  done
  return 1
}

agent_field() { herdr agent get "$1" 2>/dev/null | jq -r ".result.agent.$2 // empty"; }

# `agent start` returns when Herdr can see the agent, which is a little before the
# agent can take input — prompt into that gap and the text is swallowed with no
# error anywhere. `interactive_ready` is the flag that closes it.
wait_interactive() {
  deadline=$((SECONDS + 30))
  while [ "$SECONDS" -lt "$deadline" ]; do
    [ "$(agent_field "$1" interactive_ready)" = true ] && return 0
    sleep 0.3
  done
  return 1
}

# And a prompt that landed moves the agent off idle. If it does not, the text went
# nowhere and this step would wait for an agent that is never going to start, so
# it is worth one retry and then an honest error.
took_the_prompt() {
  deadline=$((SECONDS + 15))
  while [ "$SECONDS" -lt "$deadline" ]; do
    case "$(agent_field "$1" agent_status)" in
      working | blocked | done) return 0 ;;
    esac
    sleep 0.5
  done
  return 1
}

worktree=${SHEP_WORKTREE:-}
if [ -n "${SHEP_PANE:-}" ] && herdr pane get "$SHEP_PANE" >/dev/null 2>&1; then
  pane=$SHEP_PANE
  echo "reusing pane $pane"
else
  worktree=${worktree:-$HOME/.herdr/worktrees/$(basename "$SHEP_REPO")-$SHEP_TASK_ID}
  if [ -d "$worktree" ]; then
    echo "worktree $worktree is already there; opening it"
    created=$(herdr worktree open --cwd "$SHEP_REPO" --path "$worktree" \
                --label "$SHEP_TASK_ID" --no-focus 2>&1) \
      || die "herdr worktree open failed: $(oneline "$created")"
  else
    created=$(herdr worktree create --cwd "$SHEP_REPO" --branch "$branch" --base "$base" \
                --path "$worktree" --label "$SHEP_TASK_ID" --no-focus 2>&1) \
      || die "herdr worktree create failed: $(oneline "$created")"
  fi
  workspace=$(printf '%s' "$created" | jq -r '.result.workspace.workspace_id // empty')
  [ -n "$workspace" ] || die "herdr did not say which workspace the worktree opened in"

  # `worktree create` gives you one pane and no way to set its environment, so the
  # agent gets a split of its own — which also leaves you a shell in the worktree
  # to poke around in. $SHEP_DB is what makes `shep` in that pane talk to this
  # store; the position is deliberately not exported, because it would go stale the
  # moment the round changed and `shep check submit` reads the task row instead.
  root_pane=$(herdr pane list --workspace "$workspace" | jq -r '.result.panes[0].pane_id // empty')
  [ -n "$root_pane" ] || die "workspace $workspace has no pane to split"
  split=$(herdr pane split "$root_pane" --direction right --cwd "$worktree" --no-focus \
            --env "SHEP_DB=$SHEP_DB" --env "SHEP_TASK_ID=$SHEP_TASK_ID" --env "SHEP_BIN=$shep" 2>&1) \
    || die "herdr pane split failed: $(oneline "$split")"
  pane=$(printf '%s' "$split" | jq -r '.result.pane.pane_id // empty')
  [ -n "$pane" ] || die "herdr did not say which pane it split"

  # Bind before starting the agent: the binding is what makes the status events
  # that follow attributable to this task (PLAN §6).
  "$shep" bind-pane "$pane" --workspace "$workspace" --worktree "$worktree" \
    --branch "$branch" --base "$base" || die "could not bind pane $pane"
fi
worktree=${worktree:-$SHEP_REPO}

if herdr agent get "$pane" >/dev/null 2>&1; then
  echo "an agent is already in $pane"
else
  wait_for_shell "$pane" || die "pane $pane never came back to a shell prompt"
  started=$(herdr agent start "shep-${SHEP_TASK_ID//_/-}" --kind "$agent_kind" \
              --pane "$pane" -- $agent_flags 2>&1) \
    || die "herdr agent start failed in $pane: $(oneline "$started")"
fi

# The script owns the prompt, not the engine: this wording is the whole
# difference between an implementation step and a review one (PLAN §7.5).
read -r -d '' prompt <<PROMPT
Run \`$shep context\` — that is your brief and the whole of your instructions. You are working alone in this worktree on branch $branch; nobody will answer questions, so make the call yourself and get it done.

When the work is finished: commit it, then run

    echo "<one paragraph on what you did>" | $shep check submit --pass

or, if you could not do it, the same with --fail and the reason. That check is the only thing that tells shepherd how this went — stopping without one parks the task. Lint, tests and review run after you stop, so you do not need to ask anyone whether this is good enough.
PROMPT

wait_interactive "$pane" || die "the agent in $pane never became ready for input"

for attempt in 1 2; do
  prompted=$(herdr agent prompt "$pane" "$prompt" 2>&1) \
    || die "could not prompt the agent in $pane: $(oneline "$prompted")"
  if took_the_prompt "$pane"; then
    break
  fi
  echo "the agent in $pane is still idle after prompt $attempt"
  [ "$attempt" = 2 ] && die "the agent in $pane never picked up the prompt"
done

printf '{"outcome":"started","pane":"%s","note":%s}\n' \
  "$pane" "$(jsonstr "working in $worktree on $branch")"

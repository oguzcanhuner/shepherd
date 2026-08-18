# Shared by this repo's step scripts. Not a step itself — steps are named in
# config.toml, and nothing names this file.
#
# One trap worth naming at the top, because it cost an afternoon: macOS ships bash
# 3.2, and bash 3.2 mis-parses an apostrophe inside `${var:-default}` in a
# here-document. The quote swallows the terminator, `read` assigns nothing, and the
# prompt you meant to send is an unbound variable. So build every default *before*
# the heredoc and put plain `$var` inside it.

shep=${SHEP_BIN:-shep}

jsonstr() { printf '%s' "$1" | jq -Rs .; }
say() { printf '{"outcome":"%s","note":%s}\n' "$1" "$(jsonstr "$2")"; }
# A step reports a verdict; it does not fail. Exit 0 with `error` and let the
# engine park the task with the reason.
die() { say error "$1"; exit 0; }
oneline() { printf '%s' "$1" | tr '\n' ' ' | cut -c1-300; }

if ! command -v jq >/dev/null; then
  printf '{"outcome":"error","note":"no jq on PATH, which every step here needs"}\n'
  exit 0
fi

agent_field() { herdr agent get "$1" 2>/dev/null | jq -r ".result.agent.$2 // empty"; }

# Herdr reports an agent's status but says nothing about whether a pane can take
# one, or whether an agent can take input, or whether a prompt landed. The three
# waits below are the difference between an unattended step that works and one that
# silently never starts.

# `agent start` refuses a pane that is not "an available shell" with
# `agent_pane_busy`. Readiness is not the foreground process *group* — that is the
# shell either way, since the shell's own rc files are its children — it is the
# foreground process *list*: one entry, and that entry the shell. Probed at 3-4s of
# zsh startup on a fresh split.
wait_for_shell() {
  deadline=$((SECONDS + 30))
  while [ "$SECONDS" -lt "$deadline" ]; do
    if info=$(herdr pane process-info --pane "$1" 2>/dev/null) && [ "$(printf '%s' "$info" | jq -r '
          .result.process_info
          | (.shell_pid != null)
            and ((.foreground_processes | length) == 1)
            and (.foreground_processes[0].pid == .shell_pid)')" = true ]; then
      return 0
    fi
    sleep 0.2
  done
  return 1
}

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

# A prompt that landed moves the agent off idle. If it does not, the text went
# nowhere, and a step waiting on an agent that never started waits for ever.
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

# Send a prompt and wait for the agent to finish answering it.
#
# `agent prompt --wait` cannot do this on its own: it settles for the state the
# agent is *already* in, so prompting an idle agent returns instantly with "idle".
# Hence prompt, confirm it picked the prompt up, and only then wait.
prompt_and_wait() {
  pane=$1 text=$2 timeout=$3
  sent=$(herdr agent prompt "$pane" "$text" 2>&1) || {
    printf 'could not prompt the agent in %s: %s\n' "$pane" "$(oneline "$sent")"
    return 1
  }
  took_the_prompt "$pane" || {
    printf 'the agent in %s never picked up the prompt\n' "$pane"
    return 1
  }
  waited=$(herdr agent wait "$pane" --until idle --until done --timeout "$timeout" 2>&1) || {
    printf 'the agent in %s did not settle: %s\n' "$pane" "$(oneline "$waited")"
    return 1
  }
  return 0
}

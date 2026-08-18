#!/usr/bin/env bash
# Formatting and clippy. A warning is a rejection: this repo builds clean.
set -uo pipefail

shep=${SHEP_BIN:-shep}

fmt_out=$(cargo fmt --check 2>&1)
fmt_status=$?
clippy_out=$(cargo clippy --all-targets 2>&1)
clippy_status=$?

printf '%s\n%s\n' "$fmt_out" "$clippy_out"

problems=()
[ "$fmt_status" -ne 0 ] && problems+=("cargo fmt --check found formatting to fix")
[ "$clippy_status" -ne 0 ] && problems+=("clippy failed")
if printf '%s' "$clippy_out" | grep -qE '^warning'; then
  count=$(printf '%s' "$clippy_out" | grep -cE '^warning')
  problems+=("$count clippy warning(s)")
fi

# The verdict is also a check: a durable record of what was true of this commit,
# which is what `integrate` will insist on later (PLAN §6).
submit() {
  # Runnable by hand, which is how you debug it: with no task there is nothing to
  # write a check about.
  if [ -z "${SHEP_TASK_ID:-}" ]; then
    echo "(not running as a step, so no check was recorded)"
    return 0
  fi
  if ! printf '%s' "$2" | "$shep" check submit "$1" --author lint >/dev/null; then
    printf '{"outcome":"error","note":"lint reached a verdict but could not record it"}\n'
    exit 0
  fi
}

if [ ${#problems[@]} -gt 0 ]; then
  note=$(printf '%s; ' "${problems[@]}")
  note=${note%; }
  submit --fail "$note

$(printf '%s\n%s\n' "$fmt_out" "$clippy_out" | tail -c 3000)"
  printf '{"outcome":"reject","note":"%s"}\n' "$note"
else
  submit --pass "cargo fmt clean, cargo clippy --all-targets clean"
  printf '{"outcome":"pass","note":"fmt clean, no clippy warnings"}\n'
fi

#!/usr/bin/env bash
# The whole suite. A failure is a rejection, not an error: the tests worked, the
# code did not.
set -uo pipefail

shep=${SHEP_BIN:-shep}

output=$(cargo test --all-targets 2>&1)
status=$?
printf '%s\n' "$output"

submit() {
  # Runnable by hand, which is how you debug it: with no task there is nothing to
  # write a check about.
  if [ -z "${SHEP_TASK_ID:-}" ]; then
    echo "(not running as a step, so no check was recorded)"
    return 0
  fi
  if ! printf '%s' "$2" | "$shep" check submit "$1" --author test >/dev/null; then
    printf '{"outcome":"error","note":"the suite reached a verdict but it could not be recorded"}\n'
    exit 0
  fi
}

if [ "$status" -eq 0 ]; then
  passed=$(printf '%s' "$output" | grep -oE '^test result: ok\. [0-9]+' | grep -oE '[0-9]+' | paste -sd+ - | bc)
  submit --pass "${passed:-0} tests passed"
  printf '{"outcome":"pass","note":"%s tests passed"}\n' "${passed:-0}"
else
  failed=$(printf '%s' "$output" | grep -cE '^test .* FAILED')
  if [ "$failed" -gt 0 ]; then
    submit --fail "$failed test(s) failed

$(printf '%s' "$output" | grep -A 20 '^failures:' | tail -c 3000)"
    printf '{"outcome":"reject","note":"%s test(s) failed"}\n' "$failed"
  else
    # Nothing failed but the run did: a compile error, not a test verdict.
    submit --fail "the suite did not run to completion

$(printf '%s' "$output" | tail -c 3000)"
    printf '{"outcome":"reject","note":"the suite did not run to completion (compile error?)"}\n'
  fi
fi

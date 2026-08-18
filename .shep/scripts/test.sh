#!/usr/bin/env bash
# The whole suite. A failure is a rejection, not an error: the tests worked, the
# code did not.
set -uo pipefail

output=$(cargo test --all-targets 2>&1)
status=$?
printf '%s\n' "$output"

if [ "$status" -eq 0 ]; then
  passed=$(printf '%s' "$output" | grep -oE '^test result: ok\. [0-9]+' | grep -oE '[0-9]+' | paste -sd+ - | bc)
  printf '{"outcome":"pass","note":"%s tests passed"}\n' "${passed:-0}"
else
  failed=$(printf '%s' "$output" | grep -cE '^test .* FAILED')
  if [ "$failed" -gt 0 ]; then
    printf '{"outcome":"reject","note":"%s test(s) failed"}\n' "$failed"
  else
    # Nothing failed but the run did: a compile error, not a test verdict.
    printf '{"outcome":"reject","note":"the suite did not run to completion (compile error?)"}\n'
  fi
fi

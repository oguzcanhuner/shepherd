#!/usr/bin/env bash
# Formatting and clippy. A warning is a rejection: this repo builds clean.
set -uo pipefail

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

if [ ${#problems[@]} -gt 0 ]; then
  note=$(printf '%s; ' "${problems[@]}")
  printf '{"outcome":"reject","note":"%s"}\n' "${note%; }"
else
  printf '{"outcome":"pass","note":"fmt clean, no clippy warnings"}\n'
fi

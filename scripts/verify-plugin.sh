#!/usr/bin/env bash
# Assert that Herdr registered what herdr-plugin.toml says.
#
# Worth having as a script rather than a habit: unknown manifest fields are
# ignored silently and unknown event names only warn, so the failure mode this
# guards against is a plugin that links cleanly and never fires.
#
#   scripts/verify-plugin.sh          # check what is currently linked
#   scripts/verify-plugin.sh --link   # (re)link this checkout first, then check
set -euo pipefail

plugin_id=shepherd
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
want_events=(pane.agent_status_changed pane.exited pane.closed workspace.closed)
failures=0

fail() { printf 'FAIL  %s\n' "$1" >&2; failures=$((failures + 1)); }
pass() { printf 'ok    %s\n' "$1"; }

if [ "${1:-}" = "--link" ]; then
  if herdr plugin list --json --plugin "$plugin_id" 2>/dev/null | jq -e '.result.plugins[0]' >/dev/null; then
    herdr plugin unlink "$plugin_id" >/dev/null
  fi
  link_out=$(herdr plugin link "$root" 2>&1)
  # An unrecognised event name links anyway and only warns, so the warnings
  # array is the only place a typo shows up.
  warnings=$(printf '%s' "$link_out" | jq -r '.result.warnings // [] | .[]' 2>/dev/null || true)
  if [ -n "$warnings" ]; then
    fail "link emitted warnings:"
    printf '        %s\n' "$warnings" >&2
  else
    pass "linked with no warnings"
  fi
fi

listing=$(herdr plugin list --json --plugin "$plugin_id")
plugin=$(printf '%s' "$listing" | jq -c --arg id "$plugin_id" '.result.plugins[] | select(.plugin_id == $id)')
if [ -z "$plugin" ]; then
  fail "$plugin_id is not linked (run with --link)"
  exit 1
fi
pass "$plugin_id is linked"

[ "$(printf '%s' "$plugin" | jq -r '.enabled')" = "true" ] \
  && pass "enabled" || fail "not enabled"

got_events=$(printf '%s' "$plugin" | jq -r '.events // [] | .[].on' | sort)
for want in "${want_events[@]}"; do
  if printf '%s\n' "$got_events" | grep -qx "$want"; then
    pass "hook registered: $want"
  else
    fail "hook missing: $want (Herdr ignores what it does not understand)"
  fi
done

[ "$(printf '%s' "$plugin" | jq -r '.startup // [] | length')" -ge 1 ] \
  && pass "startup entry registered" || fail "no startup entry registered"

if [ "$failures" -gt 0 ]; then
  printf '\n%s check(s) failed. What Herdr thinks it has:\n' "$failures" >&2
  printf '%s' "$plugin" | jq . >&2
  exit 1
fi
printf '\nall checks passed\n'

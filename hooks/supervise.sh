#!/usr/bin/env bash
# Herdr's [[startup]] entry point: run the supervisor for this session.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
exec "$(shep_bin)" supervise

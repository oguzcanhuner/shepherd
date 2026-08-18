#!/usr/bin/env bash
# Append the raw Herdr event and exit. Nothing is decided here.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
exec "$(shep_bin)" forward

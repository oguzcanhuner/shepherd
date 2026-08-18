# Shared by the hooks: find the shep binary.
#
# $SHEP_BIN wins, then whichever build exists under the plugin root (release is
# what [[build]] produces; debug is what you have while working on it), then
# whatever is on PATH.
shep_bin() {
  if [ -n "${SHEP_BIN:-}" ]; then
    printf '%s\n' "$SHEP_BIN"
    return 0
  fi
  root=${HERDR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)}
  for candidate in "$root/target/release/shep" "$root/target/debug/shep"; do
    if [ -x "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  printf '%s\n' shep
}

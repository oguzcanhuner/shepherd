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

# Keep ~/.local/bin/shep pointing at this plugin's binary, so installing the
# plugin is all it takes to have `shep` on a PATH that includes ~/.local/bin.
#
# Run from the startup hook rather than a [[build]] step, on purpose: the
# managed checkout's directory name changes on every reinstall, and `plugin
# link` never runs build commands at all. A refresh at every server start
# survives both.
#
# A real file at the destination is the user's own install and wins; only a
# symlink (ours to manage) or an empty slot is touched.
expose_bin() {
  bin=$(shep_bin)
  case $bin in
    /*) [ -x "$bin" ] || return 0 ;;
    *) return 0 ;;  # bare "shep": nothing on disk to point at
  esac
  dest="$HOME/.local/bin/shep"
  if [ -e "$dest" ] && [ ! -L "$dest" ]; then
    return 0
  fi
  mkdir -p "$HOME/.local/bin"
  ln -sfn "$bin" "$dest"
}

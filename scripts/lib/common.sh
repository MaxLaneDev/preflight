# Shared shell tools. No server process or data is changed by sourcing this file.
set -eu
preflight_binary=${PREFLIGHT_BINARY:-$root/target/release/preflight}
if [ ! -x "$preflight_binary" ]; then preflight_binary=$(command -v preflight || true); fi
[ -n "$preflight_binary" ] && [ -x "$preflight_binary" ] || { echo 'Build preflight first, or set PREFLIGHT_BINARY.' >&2; exit 1; }
. "$root/scripts/lib/hash.sh"

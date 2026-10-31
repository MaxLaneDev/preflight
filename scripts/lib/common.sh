# Shared shell tools. No server process or data is changed by sourcing this file.
set -eu
preflight_binary=${PREFLIGHT_BINARY:-$root/target/release/preflight}
if [ ! -x "$preflight_binary" ]; then preflight_binary=$(command -v preflight || true); fi
[ -n "$preflight_binary" ] && [ -x "$preflight_binary" ] || { echo 'Build preflight first, or set PREFLIGHT_BINARY.' >&2; exit 1; }
sha256() {
 if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d ' ' -f 1
 else shasum -a 256 "$1" | cut -d ' ' -f 1; fi
}
require_jq() { command -v jq >/dev/null 2>&1 || { echo 'jq is required for development scripts.' >&2; exit 1; }; }

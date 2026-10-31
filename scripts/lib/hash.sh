# Portable shell helpers; no Preflight or Node executable is needed.
sha256() {
 if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d ' ' -f 1
 else shasum -a 256 "$1" | cut -d ' ' -f 1; fi
}
require_jq() { command -v jq >/dev/null 2>&1 || { echo 'jq is required for development scripts.' >&2; exit 1; }; }

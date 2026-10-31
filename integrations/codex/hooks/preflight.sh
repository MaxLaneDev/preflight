#!/bin/sh
# Hooks only add context. They never start work or approve tools.
set -eu
if command -v preflight >/dev/null 2>&1; then exec preflight codex-hook; fi
for binary in "${HOME:-}/.local/share/preflight/bin/current/preflight" /opt/homebrew/bin/preflight /usr/local/bin/preflight; do
 if [ -x "$binary" ]; then exec "$binary" codex-hook; fi
done
exit 0

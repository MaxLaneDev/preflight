#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
handlers=$(node "$root/scripts/lib/run-public.mjs" --list)
if [ -f "$root/scripts/lib/acceptance.mjs" ]; then handlers="$handlers
$(node "$root/scripts/lib/acceptance.mjs" --list)"; fi
for file in "$root"/scripts/*.sh "$root"/scripts/lib/*.sh "$root"/integrations/codex/hooks/*.sh "$root"/crates/preflight/tests/support/*.sh; do
 sh -n "$file"
 scenario=$(sed -n '/^exec node /{s/.*lib\/acceptance.mjs" \([^ ]*\) .*/\1/p;s/.*lib\/run-public.mjs" \([^ ]*\) .*/\1/p;}' "$file")
 if [ -n "$scenario" ]; then printf '%s\n' "$handlers" | grep -Fx "$scenario" >/dev/null || { echo "Unknown fixture in $file" >&2; exit 1; }; fi
done
for file in "$root"/scripts/lib/*.mjs; do node --check "$file"; done
if find "$root/scripts" "$root/integrations/codex/hooks" "$root/crates/preflight/tests/support" -name '*.py' | grep .; then echo 'Python source remains' >&2; exit 1; fi
echo 'Shell and fixture entrypoints checked'

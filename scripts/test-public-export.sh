#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' 0; trap 'exit 1' HUP INT TERM
"$root/scripts/export-public-source.sh" "$tmp/source"
for name in c-basic cpp-basic go-basic; do [ -d "$tmp/source/examples/$name" ]; done
[ "$(find "$tmp/source/examples" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')" = 3 ]
for file in build.sh check.sh; do [ -x "$tmp/source/examples/cpp-basic/$file" ]; cmp "$root/examples/cpp-basic/$file" "$tmp/source/examples/cpp-basic/$file"; done
for name in .git .validation data target preflight.toml PREFLIGHT_MASTER_PLAN.md; do [ ! -e "$tmp/source/$name" ]; done
[ "$(find "$tmp/source/migrations" -name '*.sql' | wc -l)" = "$(find "$root/migrations" -name '*.sql' | wc -l)" ]
cmp "$root/docs/status.md" "$tmp/source/docs/status.md"
if "$root/scripts/export-public-source.sh" "$tmp/source" >/dev/null 2>&1; then echo 'Overwrote export' >&2; exit 1; fi
git -C "$tmp/source" init -q; git -C "$tmp/source" add .
PREFLIGHT_EXPORT_ROOT="$tmp/source" "$root/scripts/export-public-source.sh" "$tmp/second"
cmp "$tmp/second/integrations/codex/skills/preflight-mcp/SKILL.md" "$root/skills/preflight-mcp/SKILL.md"
echo 'Public export checks passed'

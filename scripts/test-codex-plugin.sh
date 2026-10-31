#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$root/scripts/lib/common.sh"
require_jq
export PREFLIGHT_BINARY="$preflight_binary"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' 0; trap 'exit 1' HUP INT TERM
printf '{"hook_event_name":"SessionStart"}' | "$preflight_binary" codex-hook | jq -e '.hookSpecificOutput.hookEventName=="SessionStart" and (.hookSpecificOutput.additionalContext|contains("preflight-mcp"))' >/dev/null
for raw in bad '[]' null '{}' '{"hook_event_name":"Stop"}'; do [ -z "$(printf '%s' "$raw" | "$preflight_binary" codex-hook)" ]; done
awk 'BEGIN {for(i=0;i<65537;i++)printf "x"}' | "$preflight_binary" codex-hook > "$tmp/oversize"
[ ! -s "$tmp/oversize" ]
"$root/scripts/package-codex-plugin.sh" "$tmp/plugin.zip" --zip >/dev/null
unzip -Z1 "$tmp/plugin.zip" > "$tmp/names"
for name in .codex-plugin/plugin.json .mcp.json assets/icon.png hooks/preflight.sh; do grep -Fx "$name" "$tmp/names" >/dev/null; done
if grep -E '(^/|^\.\./|marketplace.json|\.py$)' "$tmp/names"; then exit 1; fi
"$root/scripts/package-codex-plugin.sh" "$tmp/marketplace" >/dev/null
plugin=$tmp/marketplace/plugins/preflight
cmp "$plugin/skills/preflight-mcp/SKILL.md" "$root/skills/preflight-mcp/SKILL.md"
cmp "$plugin/assets/icon.png" "$root/assets/branding/preflight-icon.png"
if "$root/scripts/package-codex-plugin.sh" "$tmp/marketplace" >/dev/null 2>&1; then exit 1; fi
if "$root/scripts/package-codex-plugin.sh" "$tmp/plugin.zip" --zip >/dev/null 2>&1; then exit 1; fi
# Resolve hook binaries as Codex does, with a PATH containing the freshly built CLI.
mkdir "$tmp/bin"; ln -s "$preflight_binary" "$tmp/bin/preflight"
PLUGIN_ROOT=$plugin; export PLUGIN_ROOT
jq -r '.hooks|to_entries[]|.key as $event|.value[].hooks[].command|[$event,.]|@tsv' "$plugin/hooks/hooks.json" |
while IFS="$(printf '\t')" read -r event command; do
 jq -n --arg event "$event" '{hook_event_name:$event}' | PATH="$tmp/bin:$PATH" sh -c "$command" > "$tmp/hook.json"
done
echo 'Codex plugin and hook checks passed'

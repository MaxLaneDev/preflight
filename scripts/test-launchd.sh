#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
[ "$(uname -s)" = Darwin ] || { echo 'Requires a macOS user session.' >&2; exit 1; }
scenario=launchd
if [ "$#" -gt 0 ]; then PREFLIGHT_BINARY=$1; export PREFLIGHT_BINARY; fi
. "$root/scripts/lib/acceptance.sh"
config=$out/'service & config.toml'; write_config
expected=$("$preflight_binary" --version | jq -r '.schema')
"$preflight_binary" --config "$config" service export launchd "$out/units & spaces" > "$out/export.json"
plist=$(jq -r '.files[0]' "$out/export.json")
jq -r '.files[]|select(endswith(".plist"))' "$out/export.json" > "$out/plists"
while IFS= read -r file; do plutil -lint "$file"; done < "$out/plists" > "$out/plist.log"
plutil -convert json -o "$out/plist.json" "$plist"
jq -e --arg config "$config" '.ProgramArguments[2]==$config' "$out/plist.json" >/dev/null
label=$(jq -r '.Label' "$out/plist.json"); target=gui/$(id -u)/$label; loaded=false
finish() { if [ "$loaded" = true ]; then launchctl bootout "$target"; fi; cleanup; }
trap finish 0
launchctl bootstrap "gui/$(id -u)" "$plist" > "$out/bootstrap.log" 2>&1
loaded=true
attempts=0
until curl --noproxy '*' -fsS --max-time 1 "$base/health" >/dev/null 2>&1; do
 attempts=$((attempts+1)); [ "$attempts" -lt 200 ] || { echo 'launchd readiness timed out.' >&2; exit 1; }; sleep .05
done
api capabilities | jq -e --argjson schema "$expected" '.schema_version==$schema' >/dev/null
launchctl print "$target" > "$out/status.txt"
launchctl bootout "$target"; loaded=false
attempts=0
while curl --noproxy '*' -fsS --max-time 1 "$base/health" >/dev/null 2>&1; do attempts=$((attempts+1)); [ "$attempts" -lt 100 ] || exit 1; sleep .05; done
jq -n --argjson schema "$expected" '{adapter:"launchd",ready:true,schema:$schema,path_with_spaces_and_ampersand:true}' > "$out/acceptance.json"
echo "$out"

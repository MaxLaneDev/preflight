#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
scenario=package-smoke
. "$root/scripts/lib/acceptance.sh"
set -- "${1:?package directory}"/preflight-[0-9]*.tar.gz
[ "$#" = 1 ] && [ -f "$1" ] || { echo 'Expected one server package.' >&2; exit 1; }
archive=$1; sha=$(awk 'NR==1 {print $1}' "$archive.sha256")
"$preflight_binary" package install "$archive" --sha256 "$sha" --prefix "$out/binary" --config "$config" --data "$data" > "$out/install.json"
# Preserve the generated restrictive configuration and file-backed credential.
sed "s/127.0.0.1:4170/127.0.0.1:$port/" "$config" > "$config.new"; mv "$config.new" "$config"
preflight_binary=$out/binary/current/preflight
start_server
for path in / /app.js /app.css /projects/example/pipelines /executions; do request "$path" > /dev/null; done
api capabilities | jq -e '.features.bundled_web_ui' >/dev/null
printf '{"tool":"server_capabilities","args":{}}\n' > "$out/mcp-request.json"
"$root/scripts/mcp-call.sh" "$base/mcp" "$out/mcp-request.json" > "$out/mcp-response.json"
jq -e '.features.bundled_web_ui' "$out/mcp-response.json" >/dev/null
code=$(curl --noproxy '*' -sS --max-time 10 -o "$out/denied.json" -w '%{http_code}' -H 'Content-Type: application/json' --data "$(jq -n --arg path "$project" '{name:"denied",path:$path}')" "$base/api/v1/projects")
[ "$code" = 400 ]
stop_server
# Use the build binary for uninstall, not a process inside the removed prefix.
"$root/scripts/release.sh" uninstall --prefix "$out/binary" > "$out/uninstall.json"
[ -f "$config" ] && [ -f "$data/preflight.sqlite3" ]
echo "Packaged UI/API cold start passed: $out"

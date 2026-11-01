#!/bin/sh
# Exercise the published installer in a disposable prefix, never the user's service.
set -eu
version=${1:?stable tag or latest}
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
case "$version" in latest) base=https://github.com/MaxLaneDev/preflight/releases/latest/download;;
 *) printf '%s\n' "$version" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$' || exit 1
    base=https://github.com/MaxLaneDev/preflight/releases/download/$version;; esac
umask 077
temporary=$(mktemp -d)
pid=''
cleanup() { if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; wait "$pid" || true; fi; rm -rf "$temporary"; }
trap cleanup 0
trap 'exit 1' HUP INT TERM
curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --max-time 180 "$base/install.sh" -o "$temporary/install.sh"
export PREFLIGHT_PREFIX="$temporary/binary with spaces" PREFLIGHT_CONFIG="$temporary/settings/preflight.toml" PREFLIGHT_DATA="$temporary/data" PREFLIGHT_VERSION="$version"
sh "$temporary/install.sh" > "$temporary/receipt.json"
binary=$PREFLIGHT_PREFIX/current/preflight
"$binary" --version > "$temporary/version.json"
if [ "$version" != latest ]; then jq -e --arg version "${version#v}" '.version==$version' "$temporary/version.json" >/dev/null; fi
cp "$binary" "$temporary/verifier"
port=$("$binary" package free-port)
sed "s/127.0.0.1:4170/127.0.0.1:$port/" "$PREFLIGHT_CONFIG" > "$temporary/config.new"
mv "$temporary/config.new" "$PREFLIGHT_CONFIG"
"$binary" --config "$PREFLIGHT_CONFIG" serve > "$temporary/server.log" 2>&1 & pid=$!
attempt=0
while ! curl --noproxy '*' --silent --fail --max-time 1 "http://127.0.0.1:$port/health" >/dev/null; do
 kill -0 "$pid"; attempt=$((attempt+1)); [ "$attempt" -lt 200 ] || { tail -n 40 "$temporary/server.log"; exit 1; }; sleep .1
done
for path in / /app.js /app.css /executions; do curl --noproxy '*' --silent --show-error --fail --max-time 10 "http://127.0.0.1:$port$path" >/dev/null; done
curl --noproxy '*' --silent --show-error --fail --max-time 10 "http://127.0.0.1:$port/api/v1/capabilities" | jq -e '.features.bundled_web_ui' >/dev/null
printf '{"tool":"server_capabilities","args":{}}\n' > "$temporary/request.json"
PREFLIGHT_BINARY="$binary" sh "$root/scripts/mcp-call.sh" "http://127.0.0.1:$port/mcp" "$temporary/request.json" | jq -e '.features.bundled_web_ui' >/dev/null
kill "$pid"; wait "$pid" || true; pid=''
"$temporary/verifier" package uninstall --prefix "$PREFLIGHT_PREFIX" > "$temporary/uninstall.json"
[ ! -e "$PREFLIGHT_PREFIX" ] && [ -f "$PREFLIGHT_CONFIG" ] && [ -f "$PREFLIGHT_DATA/preflight.sqlite3" ]
echo "Published installer, native binary, Web UI and MCP passed: $version $(uname -s) $(uname -m)"

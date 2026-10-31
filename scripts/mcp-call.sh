#!/bin/sh
# One bounded call to an explicitly selected loopback acceptance server.
set -eu
endpoint=${1:?Usage: mcp-call.sh MCP_URL REQUEST.json}
input=${2:?request file containing tool and args}
[ "$(printf '%s\n' "$endpoint" | wc -l | tr -d ' ')" = 1 ] || { echo 'Invalid endpoint.' >&2; exit 1; }
printf '%s\n' "$endpoint" | grep -Eq '^http://(127\.0\.0\.1|localhost|\[::1\])(:[0-9]+)?/mcp/?$' || { echo 'Acceptance endpoint must be local HTTP /mcp.' >&2; exit 1; }
command -v jq >/dev/null
[ "$(wc -c < "$input")" -le 2097152 ] || { echo 'Request exceeds 2 MiB.' >&2; exit 1; }
jq -e '.tool|type=="string"' "$input" >/dev/null
jq -e '.args|type=="object"' "$input" >/dev/null
umask 077; tmp=$(mktemp -d); session=''; version=2025-06-18; curl_pid=''
cleanup() {
 if [ -n "$curl_pid" ]; then kill "$curl_pid" 2>/dev/null || true; wait "$curl_pid" 2>/dev/null || true; fi
 if [ -n "$session" ]; then curl --noproxy '*' --silent --max-time 3 -X DELETE -H "Mcp-Session-Id: $session" -H "MCP-Protocol-Version: $version" "$endpoint" >/dev/null 2>&1 || true; fi
 rm -rf "$tmp"
}
trap cleanup 0; trap 'exit 1' HUP INT TERM
mkfifo "$tmp/stream"
matched() {
 printf '%s\n' "$1" | jq -e --argjson id "$expected_id" 'type=="object" and .id==$id' >/dev/null 2>&1 || return 1
 printf '%s\n' "$1" > "$tmp/reply.json"
}
send() {
 message=$1; expected_id=${2:-}; data=''; body=''
 rm -f "$tmp/reply.json" "$tmp/headers"
 if [ -n "$session" ]; then
  curl --noproxy '*' --fail --silent --show-error --no-buffer --max-time 30 --max-filesize 2097152 -D "$tmp/headers" -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' -H "Mcp-Session-Id: $session" -H "MCP-Protocol-Version: $version" --data-binary "@$message" "$endpoint" > "$tmp/stream" 2> "$tmp/curl-error" &
 else
  curl --noproxy '*' --fail --silent --show-error --no-buffer --max-time 30 --max-filesize 2097152 -D "$tmp/headers" -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' --data-binary "@$message" "$endpoint" > "$tmp/stream" 2> "$tmp/curl-error" &
 fi
 curl_pid=$!
 while IFS= read -r line || [ -n "$line" ]; do
  [ -n "$expected_id" ] || continue
  line=$(printf '%s' "$line" | tr -d '\r')
  case "$line" in
   data:*) part=${line#data:}; part=${part# }; if [ -n "$data" ]; then data="$data
$part"; else data=$part; fi;;
   '') if [ -n "$data" ] && matched "$data"; then break; fi; data='';;
   event:*|id:*|retry:*|:*) :;;
   *) body="$body$line"; if matched "$body"; then break; fi;;
  esac
 done < "$tmp/stream"
 if [ -f "$tmp/reply.json" ]; then kill "$curl_pid" 2>/dev/null || true; wait "$curl_pid" 2>/dev/null || true
 else
  if ! wait "$curl_pid"; then cat "$tmp/curl-error" >&2; curl_pid=''; return 1; fi
  if [ -n "$expected_id" ]; then echo 'MCP response missing or ID mismatch.' >&2; curl_pid=''; return 1; fi
 fi
 curl_pid=''
 awk '/^HTTP\// {status=$2} END {if(status!~/^2[0-9][0-9]$/)exit 1}' "$tmp/headers"
 next_session=$(awk 'tolower($1)=="mcp-session-id:" {gsub("\r","",$2);value=$2} END {print value}' "$tmp/headers")
 if [ -n "$next_session" ]; then
  [ "${#next_session}" -le 512 ] || return 1
  case "$next_session" in *[!A-Za-z0-9._-]*) echo 'Invalid MCP session.' >&2; return 1;; esac
  session=$next_session
 fi
 if [ -n "$expected_id" ]; then
  if jq -e '.error!=null' "$tmp/reply.json" >/dev/null; then jq -c '.error' "$tmp/reply.json" >&2; return 1; fi
  jq -e 'has("result")' "$tmp/reply.json" >/dev/null
 fi
}
jq -n --arg version "$version" '{jsonrpc:"2.0",id:1,method:"initialize",params:{protocolVersion:$version,capabilities:{},clientInfo:{name:"preflight-acceptance",version:"1"}}}' > "$tmp/request.json"
send "$tmp/request.json" 1
version=$(jq -er '.result.protocolVersion' "$tmp/reply.json")
[ "${#version}" = 10 ] || exit 1
printf '%s\n' "$version" | grep -Eq '^[0-9]{4}-[0-9]{2}-[0-9]{2}$' || exit 1
printf '{"jsonrpc":"2.0","method":"notifications/initialized"}\n' > "$tmp/request.json"
send "$tmp/request.json"
jq '{jsonrpc:"2.0",id:2,method:"tools/call",params:{name:.tool,arguments:.args}}' "$input" > "$tmp/request.json"
send "$tmp/request.json" 2
if jq -e '.result.isError==true' "$tmp/reply.json" >/dev/null; then jq -r '.result.content[]|select(.type=="text")|.text' "$tmp/reply.json" >&2; exit 1; fi
jq -r '.result.content[]|select(.type=="text")|.text' "$tmp/reply.json"

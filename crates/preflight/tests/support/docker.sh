#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
state=$root/container.json
[ "${1:-}" != --host ] || shift 2
case "${1:-} ${2:-}" in
 'context inspect')
  printf '"unix:///preflight-test-only.sock"\n'
  if [ -f "$root/rotate-runtime" ]; then rm "$root/rotate-runtime"; printf '\n# rotated runtime\n' >> "$0"; fi;;
 'container create')
  shift 2; labels='{}'
  while [ "$#" -gt 0 ]; do
   if [ "$1" = --label ]; then key=${2%%=*}; value=${2#*=}; labels=$(printf '%s' "$labels" | jq --arg k "$key" --arg v "$value" '.+{($k):$v}'); shift 2; else shift; fi
  done
  owner=$(printf '%s' "$labels" | jq -r '."io.preflight.owner"')
  if command -v sha256sum >/dev/null 2>&1; then cid=$(printf '%s' "$owner" | sha256sum | cut -d ' ' -f 1); else cid=$(printf '%s' "$owner" | shasum -a 256 | cut -d ' ' -f 1); fi
  jq -n --arg id "$cid" --argjson labels "$labels" '{id:$id,labels:$labels,image:("sha256:"+("a"*64)),state:{Running:false,Status:"created",ExitCode:0}}' > "$state"
  if [ -f "$root/uncertain-create" ]; then mv "$state" "$root/pending-container.json"; echo 'create outcome unknown' >&2; exit 1; fi
  echo "$cid";;
 'container ls') if [ -f "$state" ]; then jq -r '.id' "$state"; fi;;
 'container inspect') if [ -f "$root/foreign-label" ]; then jq '.labels["io.preflight.owner"]="foreign"' "$state"; else cat "$state"; fi;;
 'image inspect') jq -n '{id:("sha256:"+("a"*64)),os:"linux",arch:"arm64"}';;
 'container start') jq '.state={Running:false,Status:"exited",ExitCode:0,OOMKilled:false,StartedAt:"2026-10-30T00:00:00Z",FinishedAt:"2026-10-30T00:00:01Z"}' "$state" > "$state.new"; mv "$state.new" "$state"; echo 'fixture output';;
 'container rm') [ ! -f "$root/refuse-remove" ] || { echo 'remove refused' >&2; exit 1; }; rm -f "$state"; for last do :; done; echo "$last";;
 *) echo 'unsupported fixture invocation' >&2; exit 1;;
esac

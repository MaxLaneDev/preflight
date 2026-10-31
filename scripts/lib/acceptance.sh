# Isolated acceptance fixtures; source after setting root and scenario.
. "$root/scripts/lib/common.sh"
require_jq
command -v node >/dev/null
umask 077
mkdir -p "$root/.validation"
out=$(mktemp -d "$root/.validation/$scenario-XXXXXXXX")
project=$out/project; mkdir "$project"
data=$out/data; config=$out/preflight.toml
pids=''
cleanup() {
 for pid in $pids; do
  if kill -0 "$pid" 2>/dev/null; then
   kill "$pid" 2>/dev/null || true
   attempts=0
   while kill -0 "$pid" 2>/dev/null && [ "$attempts" -lt 100 ]; do sleep .1; attempts=$((attempts+1)); done
   if kill -0 "$pid" 2>/dev/null; then kill -KILL "$pid" 2>/dev/null || true; fi
  fi
  wait "$pid" 2>/dev/null || true
 done
}
trap cleanup 0
trap 'exit 1' HUP INT TERM
free_port() { node --input-type=module -e 'import net from "node:net"; const s=net.createServer();s.listen(0,"127.0.0.1",()=>{console.log(s.address().port);s.close()});'; }
port=$(free_port); base=http://127.0.0.1:$port
json_string() { jq -Rn --arg value "$1" '$value'; }
write_config() {
 printf 'version=1\nbind="127.0.0.1:%s"\ndata_dir=%s\nallowed_roots=[%s]\nmax_parallel_commands=4\n' "$port" "$(json_string "$data")" "$(json_string "$project")" > "$config"
}
ready() {
 attempts=0
 while [ "$attempts" -lt 200 ]; do
  kill -0 "$1" 2>/dev/null || { echo 'Owned process exited; inspect fixture logs.' >&2; return 1; }
  if curl --noproxy '*' --silent --fail --max-time 1 "$2" >/dev/null; then return; fi
  sleep .05; attempts=$((attempts+1))
 done
 echo 'Owned process readiness timed out.' >&2; return 1
}
start_server() { "$preflight_binary" --config "$config" serve >> "$out/server.log" 2>&1 & server_pid=$!; pids="$server_pid $pids"; ready "$server_pid" "$base/health"; }
stop_server() { kill "$server_pid"; wait "$server_pid" || true; pids=$(printf '%s\n' "$pids" | sed "s/$server_pid//"); }
request() {
 path=$1; shift
 if [ "$#" -gt 0 ]; then curl --noproxy '*' --silent --show-error --fail --max-time 30 -H 'Content-Type: application/json' --data "$1" "$base$path"
 else curl --noproxy '*' --silent --show-error --fail --max-time 30 "$base$path"; fi
}
api() { path=$1; shift; request "/api/v1/$path" "$@"; }
register_project() { api projects "$(jq -n --arg name "${1:-Owned acceptance}" --arg path "$project" '{name:$name,path:$path}')" | jq -er '.id'; }
wait_run() {
 attempts=0
 while [ "$attempts" -lt 600 ]; do
  api "runs/$1" > "$out/observed.json"
  if jq -e '.finished_at!=null' "$out/observed.json" >/dev/null; then cat "$out/observed.json"; return; fi
  sleep .05; attempts=$((attempts+1))
 done
 echo 'Owned run timed out.' >&2; return 1
}
start_client() {
 client_port=$(free_port); client=http://127.0.0.1:$client_port
 "${PREFLIGHT_CLIENT_BINARY:-$root/target/release/preflight-client}" --server "$base" --bind "127.0.0.1:$client_port" >> "$out/client.log" 2>&1 & client_pid=$!; pids="$client_pid $pids"; ready "$client_pid" "$client"
}
browser() { node "$root/scripts/lib/run-bounded.mjs" "${BROWSER_TIMEOUT:-180}" "$out/browser.log" node "$root/scripts/$1" "$out/fixture.json" "$out"; }

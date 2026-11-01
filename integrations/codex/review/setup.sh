#!/bin/sh
# Prepare sample inputs only. Never starts a server, registers a project or pushes Git.
set -eu
umask 077
review_source=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
review_destination=${1:?Usage: sh review/setup.sh NEW_DIRECTORY [PORT]}
review_port=${2:-49170}
case "$review_port" in ''|*[!0-9]*) echo 'PORT must be an integer.' >&2; exit 1;; esac
[ "$review_port" -ge 1024 ] && [ "$review_port" -le 65535 ] || exit 1
[ ! -e "$review_destination" ] && [ ! -L "$review_destination" ] || { echo 'Destination exists.' >&2; exit 1; }
command -v git >/dev/null
command -v jq >/dev/null
# Fixture preparation must not run the operator's global Git hooks or signing agent.
review_git() { git -c core.hooksPath=/dev/null -c commit.gpgsign=false "$@"; }
mkdir -p "$(dirname -- "$review_destination")"
mkdir "$review_destination"
review_destination=$(CDPATH= cd -- "$review_destination" && pwd)
review_project=$review_destination/project
mkdir "$review_project"
cp "$review_source/main.c" "$review_project/main.c"
cp "$review_source/pipeline.json" "$review_destination/pipeline.json"
review_git -C "$review_project" init -q
review_git -C "$review_project" add -- main.c
review_git -C "$review_project" -c user.name='Preflight review fixture' -c user.email=review@example.invalid commit -qm 'Review baseline'
review_base=$(review_git -C "$review_project" rev-parse HEAD)
for review_variant in a b; do
 sed "s/review-base/review-$review_variant/" "$review_source/main.c" > "$review_project/main.c"
 review_git --no-pager -C "$review_project" diff --no-ext-diff --no-textconv -- main.c > "$review_destination/change-$review_variant.patch"
done
sed 's/return 0;/return preflight_review_unknown_symbol;/' "$review_source/main.c" > "$review_project/main.c"
review_git --no-pager -C "$review_project" diff --no-ext-diff --no-textconv -- main.c > "$review_destination/compiler-error.patch"
cp "$review_source/main.c" "$review_project/main.c"
review_hash() {
 if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d ' ' -f 1
 else shasum -a 256 "$1" | cut -d ' ' -f 1; fi
}
jq -n --arg root "$review_project" --arg base "$review_base" --arg out "$review_destination" \
 --arg a "$(review_hash "$review_destination/change-a.patch")" \
 --arg b "$(review_hash "$review_destination/change-b.patch")" \
 --arg error "$(review_hash "$review_destination/compiler-error.patch")" \
 '{project_root:$root,base_commit:$base,pipeline:($out+"/pipeline.json"),patches:[{id:"a",path:($out+"/change-a.patch"),sha256:$a},{id:"b",path:($out+"/change-b.patch"),sha256:$b}],compiler_error_patch:{path:($out+"/compiler-error.patch"),sha256:$error}}' > "$review_destination/inputs.json"
printf 'version=1\nbind="127.0.0.1:%s"\ndata_dir=%s\nallowed_roots=[%s]\nmax_parallel_commands=2\n' \
 "$review_port" "$(jq -Rn --arg v "$review_destination/data" '$v')" "$(jq -Rn --arg v "$review_project" '$v')" > "$review_destination/server.toml"
printf '%s\n' '{"version":1,"pipeline":{"stages":[{"id":"wait","command":{"program":"sh","args":["-c","sleep 60"],"timeout_seconds":90}}]}}' > "$review_destination/cancellation-pipeline.json"
jq -n --arg config "$review_destination/server.toml" --arg inputs "$review_destination/inputs.json" --arg endpoint "http://127.0.0.1:$review_port/mcp" '{server_config:$config,inputs:$inputs,mcp_endpoint:$endpoint,server_started:false,project_registered:false}'

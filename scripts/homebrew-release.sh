#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$root/scripts/lib/common.sh"
assets=${1:?assets directory}; tag=${2:?release tag}; destination=${3:?formula path}
printf '%s\n' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$' || { echo 'Stable release tag required.' >&2; exit 1; }
[ "$(wc -c < "$assets/SHA256SUMS")" -le 1048576 ] || exit 1
name=preflight-${tag#v}-macos-aarch64.tar.gz
sha=$(awk -v name="$name" 'length($1)!=64 || $1~/[^0-9a-f]/ || $0!~/^[0-9a-f]+  [A-Za-z0-9_.-]+$/ || seen[$2]++ {bad=1} $2==name {sha=$1;count++} END {if(bad||count!=1)exit 1;print sha}' "$assets/SHA256SUMS")
require_jq
"$root/scripts/release.sh" verify "$assets/$name" --sha256 "$sha" | jq -e --arg version "${tag#v}" '.metadata.version==$version' >/dev/null
exec "$root/scripts/release.sh" formula "$assets/$name" --sha256 "$sha" --url "https://github.com/MaxLaneDev/preflight/releases/download/$tag/$name" --homepage https://github.com/MaxLaneDev/preflight --destination "$destination"

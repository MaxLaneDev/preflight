#!/bin/sh
# Export only reviewed source paths. Never copy local history or runtime data.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
source_root=${PREFLIGHT_EXPORT_ROOT:-$root}
destination=${1:?new export directory}
[ ! -e "$destination" ] || { echo 'Destination exists.' >&2; exit 1; }
command -v node >/dev/null
# Selection and boundary checks run before creating the destination.
node "$root/scripts/lib/export-source.mjs" "$source_root" "$destination"

#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$root/scripts/lib/common.sh"
exec "$preflight_binary" package export "${1:?new export directory}" --repository "${PREFLIGHT_EXPORT_ROOT:-$root}"

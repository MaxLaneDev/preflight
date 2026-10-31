#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$root/scripts/lib/common.sh"
operation=${1:?Usage: release.sh create|verify|install|status|rollback|uninstall|formula ...}
shift
case "$operation" in package) operation=create;; esac
case "$operation" in create|formula|plugin) set -- "$@" --repository "$root";; esac
exec "$preflight_binary" package "$operation" "$@"

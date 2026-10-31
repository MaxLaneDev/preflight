#!/bin/sh
# Use only with a package produced by trusted CI or a public release.
set -eu
archive=${1:?archive}; output=${2:?new binary path}
[ ! -e "$output" ] || { echo 'Output exists.' >&2; exit 1; }
expected=$(awk 'NR==1 {print $1}' "$archive.sha256")
[ "${#expected}" = 64 ] || exit 1
case "$expected" in *[!0-9a-f]*) exit 1;; esac
if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$archive" | cut -d ' ' -f 1); else actual=$(shasum -a 256 "$archive" | cut -d ' ' -f 1); fi
[ "$expected" = "$actual" ] || { echo 'Archive checksum mismatch.' >&2; exit 1; }
(set -C; umask 077; tar -xOzf "$archive" preflight > "$output")
chmod 700 "$output"

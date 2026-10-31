#!/bin/sh
# Own a bounded disk image. Never fill the host volume.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
[ "$(uname -s)" = Darwin ] || { echo 'Requires macOS hdiutil.' >&2; exit 1; }
umask 077
out=$(mktemp -d "$root/.validation/storage-volume-XXXXXXXX")
tmp=$(mktemp -d); attached=false
cleanup() { if [ "$attached" = true ]; then hdiutil detach "$tmp/mount" >> "$out/volume.log" 2>&1; fi; rm -rf "$tmp"; }
trap cleanup 0; trap 'exit 1' HUP INT TERM
mkdir "$tmp/mount"
hdiutil create -size 64m -fs HFS+ -volname PreflightStorageQA "$tmp/owned.dmg" > "$out/volume.log" 2>&1
hdiutil attach -nobrowse -mountpoint "$tmp/mount" "$tmp/owned.dmg" >> "$out/volume.log" 2>&1
attached=true
cd "$root"
PREFLIGHT_TEST_VOLUME="$tmp/mount" cargo test --release -p preflight --test evidence_retention --test storage_gc --locked --offline -- --ignored --test-threads=1 --nocapture >> "$out/volume.log" 2>&1
grep -F 'full_volume_evidence ' "$out/volume.log" > /dev/null
grep -F 'real_disk_pressure_evidence ' "$out/volume.log" > /dev/null
hdiutil detach "$tmp/mount" >> "$out/volume.log" 2>&1
attached=false
printf '{"disposable_volume_mib":64,"enospc_test":"passed","watermark_test":"passed","volume_detached":true}\n' > "$out/acceptance.json"
echo "$out"

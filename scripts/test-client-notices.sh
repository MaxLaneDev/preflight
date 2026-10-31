#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$root/scripts/lib/hash.sh"
require_jq
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' 0; trap 'exit 1' HUP INT TERM
client=$tmp/client
mkdir -p "$client/src" "$client/node_modules/@test/pkg"
printf 'export const bundled = true;\n' > "$client/src/editor-bundle.mjs"
printf '{"packages":{"node_modules/@test/pkg":{"version":"1.2.3"},"node_modules/dev-only":{"version":"9","dev":true}}}\n' > "$client/package-lock.json"
printf '{"name":"@test/pkg","version":"1.2.3","license":"MIT"}\n' > "$client/node_modules/@test/pkg/package.json"
printf 'License text with "quotes" and Unicode: ✓\n' > "$client/node_modules/@test/pkg/LICENSE"
sh "$root/scripts/client-notices.sh" "$client" > /dev/null
jq -e --rawfile license "$client/node_modules/@test/pkg/LICENSE" --arg lock "$(sha256 "$client/package-lock.json")" --arg bundle "$(sha256 "$client/src/editor-bundle.mjs")" '.format=="preflight-js-notices-v1" and .lock_sha256==$lock and .bundle_sha256==$bundle and (.packages|length)==1 and .packages[0].name=="@test/pkg" and .files["licenses/npm/@test_pkg-1.2.3/LICENSE"]==$license' "$client/THIRD_PARTY_NOTICES.json" >/dev/null
cp "$client/THIRD_PARTY_NOTICES.json" "$tmp/retained.json"
printf '{"name":"@test/pkg","version":"wrong"}\n' > "$client/node_modules/@test/pkg/package.json"
if sh "$root/scripts/client-notices.sh" "$client" >/dev/null 2>&1; then exit 1; fi
cmp "$tmp/retained.json" "$client/THIRD_PARTY_NOTICES.json"
printf '{"name":"@test/pkg","version":"1.2.3","license":"MIT"}\n' > "$client/node_modules/@test/pkg/package.json"
rm "$client/node_modules/@test/pkg/LICENSE"
if sh "$root/scripts/client-notices.sh" "$client" >/dev/null 2>&1; then exit 1; fi
cmp "$tmp/retained.json" "$client/THIRD_PARTY_NOTICES.json"
echo 'Shell license inventory checks passed'

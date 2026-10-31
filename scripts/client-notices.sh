#!/bin/sh
# Copy licenses for the pinned editor bundle. No downloads or Node runtime needed.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$root/scripts/lib/hash.sh"
require_jq
client=${1:-$root/crates/preflight-client}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' 0; trap 'exit 1' HUP INT TERM
printf '{}\n' > "$tmp/files.json"
: > "$tmp/packages.jsonl"
jq -er '.packages|to_entries[]|select(.key|startswith("node_modules/"))|select(.value.dev!=true)|[.key,.value.version]|@tsv' "$client/package-lock.json" > "$tmp/dependencies"
while IFS="$(printf '\t')" read -r relative version; do
 case "$relative" in *..*|/*|*\\*) echo 'Unsafe dependency path.' >&2; exit 1;; esac
 directory=$client/$relative
 [ ! -L "$directory" ] || { echo 'Dependency must not be a symlink.' >&2; exit 1; }
 jq -e --arg version "$version" '.version==$version and (.name|type=="string")' "$directory/package.json" >/dev/null
 name=$(jq -r '.name' "$directory/package.json")
 package_key=$(printf '%s' "$name" | tr '/' '_')
 printf '[]\n' > "$tmp/copies.json"
 for file in "$directory"/*; do
  [ -f "$file" ] && [ ! -L "$file" ] || continue
  filename=$(basename "$file")
  folded=$(printf '%s' "$filename" | tr '[:upper:]' '[:lower:]')
  case "$folded" in license*|licence*|copying*|notice*) :;; *) continue;; esac
  key=licenses/npm/$package_key-$version/$filename
  jq --arg key "$key" --rawfile text "$file" '.+{($key):$text}' "$tmp/files.json" > "$tmp/next.json"; mv "$tmp/next.json" "$tmp/files.json"
  jq --arg key "$key" '.+[$key]' "$tmp/copies.json" > "$tmp/next.json"; mv "$tmp/next.json" "$tmp/copies.json"
 done
 jq -e 'length>0' "$tmp/copies.json" >/dev/null || { echo "Missing license text: $name" >&2; exit 1; }
 jq --slurpfile files "$tmp/copies.json" '{ecosystem:"npm",name,version,license,files:$files[0]}' "$directory/package.json" >> "$tmp/packages.jsonl"
done < "$tmp/dependencies"
jq -s '.' "$tmp/packages.jsonl" > "$tmp/packages.json"
jq -n --arg lock "$(sha256 "$client/package-lock.json")" --arg bundle "$(sha256 "$client/src/editor-bundle.mjs")" --slurpfile packages "$tmp/packages.json" --slurpfile files "$tmp/files.json" '{format:"preflight-js-notices-v1",lock_sha256:$lock,bundle_sha256:$bundle,packages:$packages[0],files:$files[0]}' > "$tmp/notices.json"
cat "$tmp/notices.json" > "$client/THIRD_PARTY_NOTICES.json"
jq '{packages:(.packages|length),license_files:(.files|length)}' "$tmp/notices.json"

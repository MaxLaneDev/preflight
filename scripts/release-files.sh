#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$root/scripts/lib/common.sh"
require_jq
assets=${1:?assets directory}; destination=${2:?new destination}; tag=${3:-}
if [ -n "$tag" ]; then printf '%s\n' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$' || exit 1; fi
[ ! -e "$destination" ] || { echo 'Destination exists.' >&2; exit 1; }
requested_destination=$destination
parent=$(dirname "$destination")
mkdir -p "$parent"
destination=$(mktemp -d "$parent/.preflight-release-XXXXXXXX")
trap 'rm -rf "$destination"' 0
trap 'exit 1' HUP INT TERM
find "$assets" -type f -name 'preflight-[0-9]*.tar.gz' > "$destination/.archives"
[ -s "$destination/.archives" ] || { echo 'No server packages.' >&2; exit 1; }
count=0
while IFS= read -r archive; do
 name=$(basename "$archive"); [ ! -e "$destination/$name" ] || { echo 'Duplicate release filename.' >&2; exit 1; }
 sha=$(awk 'NR==1 {print $1}' "$archive.sha256")
 "$root/scripts/release.sh" verify "$archive" --sha256 "$sha" > "$destination/.manifest"
 jq -e '.component=="preflight"' "$destination/.manifest" >/dev/null
 version=$(jq -r '.metadata.version' "$destination/.manifest")
 [ -z "$tag" ] || [ "$tag" = "v$version" ] || { echo 'Tag differs from package.' >&2; exit 1; }
 installer=$(dirname "$archive")/install-preflight.sh
 if [ -e "$destination/install-preflight.sh" ]; then cmp "$installer" "$destination/install-preflight.sh"; else cp "$installer" "$destination/install-preflight.sh"; fi
 cp "$archive" "$archive.sha256" "$destination/"
 jq -r '.metadata.os+" "+.metadata.arch' "$destination/.manifest" >> "$destination/.platforms"
 if jq -e '.metadata.arch=="aarch64" and (.metadata.os=="macos" or .metadata.os=="linux")' "$destination/.manifest" >/dev/null; then
  platform=$(jq -r '.metadata.os+"-"+.metadata.arch' "$destination/.manifest")
  "$preflight_binary" package plugin-stdio "$archive" --sha256 "$sha" --repository "$root" --destination "$destination/preflight-stdio-plugin-$version-$platform.zip" >/dev/null
 fi
 if jq -e '.metadata.os=="macos" and .metadata.arch=="aarch64"' "$destination/.manifest" >/dev/null; then
  "$root/scripts/release.sh" formula "$archive" --sha256 "$sha" --url "https://github.com/MaxLaneDev/preflight/releases/download/v$version/$name" --homepage https://github.com/MaxLaneDev/preflight --destination "$destination/preflight.rb" >/dev/null
 fi
 count=$((count+1))
done < "$destination/.archives"
cp "$root/install.sh" "$destination/install.sh"
plugin_version=$(jq -r '.version' "$root/integrations/codex/.codex-plugin/plugin.json")
"$root/scripts/package-codex-plugin.sh" "$destination/preflight-codex-plugin-$plugin_version.zip" --zip >/dev/null
{
 printf 'Preflight includes the Web UI and MCP in one server.\n\nBuilt and checked on:\n'
 sed 's/^/- /' "$destination/.platforms"
 printf '\nCheck SHA256SUMS before installation. Services are never restarted by the installer. Configuration and data stay outside the binary installation.\n\nWindows/x86_64 and NAT traversal remain deferred.\n'
} > "$destination/RELEASE_NOTES.md"
# Include release notes in the checksums too.
for file in "$destination"/*; do [ -f "$file" ] || continue; printf '%s  %s\n' "$(sha256 "$file")" "$(basename "$file")"; done > "$destination/.sums"
mv "$destination/.sums" "$destination/SHA256SUMS"
rm "$destination/.archives" "$destination/.manifest" "$destination/.platforms"
mv "$destination" "$requested_destination"
jq -n --argjson count "$count" '{packages:$count,draft_only:true}'

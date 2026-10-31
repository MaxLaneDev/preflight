#!/bin/sh
# Distribution integration contracts; Rust unit tests cover hostile archive payloads.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$root/scripts/lib/common.sh"
require_jq
export PREFLIGHT_BINARY="$preflight_binary"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' 0; trap 'exit 1' HUP INT TERM
# A harmless executable fixture makes macOS formula checks portable to Linux CI.
cat > "$tmp/server" <<'FIXTURE'
#!/bin/sh
[ "$1" = --version ] || exit 1
printf '{"component":"preflight","version":"0.1.0","protocol":"fixture","schema":53,"os":"macos","arch":"aarch64"}\n'
FIXTURE
chmod +x "$tmp/server"
"$root/scripts/release.sh" package --server "$tmp/server" --destination "$tmp/package" > "$tmp/package.json"
"$root/scripts/release.sh" package --server "$tmp/server" --destination "$tmp/repeat" > "$tmp/repeat.json"
[ "$(jq -r '.[0].sha256' "$tmp/package.json")" = "$(jq -r '.[0].sha256' "$tmp/repeat.json")" ]
"$root/scripts/release-files.sh" "$tmp/package" "$tmp/release" v0.1.0 > "$tmp/release.json"
[ -f "$tmp/release/SHA256SUMS" ] && [ -f "$tmp/release/install-preflight.sh" ]
sha=$(jq -r '.[0].sha256' "$tmp/package.json")
grep -F "$sha" "$tmp/release/preflight.rb" >/dev/null
[ "$(awk '$2=="preflight.rb" {print $1}' "$tmp/release/SHA256SUMS")" = "$(sha256 "$tmp/release/preflight.rb")" ]
if "$root/scripts/release-files.sh" "$tmp/package" "$tmp/bad-tag" v9.0.0 >/dev/null 2>&1; then exit 1; fi
[ ! -e "$tmp/bad-tag" ]
"$root/scripts/homebrew-release.sh" "$tmp/release" v0.1.0 "$tmp/formula.rb" >/dev/null
grep -F "$sha" "$tmp/formula.rb" >/dev/null
for tag in v9.0.0 'v0.1.0;bad'; do
 if "$root/scripts/homebrew-release.sh" "$tmp/release" "$tag" "$tmp/bad.rb" >/dev/null 2>&1; then exit 1; fi
 [ ! -e "$tmp/bad.rb" ]
done
if "$root/scripts/homebrew-release.sh" "$tmp/release" v0.1.0 "$tmp/formula.rb" >/dev/null 2>&1; then exit 1; fi
printf '%064d\n' 0 > "$tmp/package/preflight-0.1.0-macos-aarch64.tar.gz.sha256"
if "$root/scripts/release-files.sh" "$tmp/package" "$tmp/bad-hash" v0.1.0 >/dev/null 2>&1; then exit 1; fi
[ ! -e "$tmp/bad-hash" ]
echo 'Package reproducibility, release collection and Homebrew checks passed'

#!/bin/sh
# Bootstrap only a checksum-verified release binary. It verifies and installs the archive.
set -eu
command -v curl >/dev/null 2>&1 || { echo 'curl is required.' >&2; exit 1; }
case "$(uname -s)" in Darwin) os=macos;; Linux) os=linux;; *) echo 'Use install.ps1 on Windows; this installer supports macOS and Linux.' >&2; exit 1;; esac
case "$(uname -m)" in arm64|aarch64) arch=aarch64;; x86_64|amd64) arch=x86_64;; *) echo 'Unsupported architecture.' >&2; exit 1;; esac
version=${PREFLIGHT_VERSION:-latest}
if [ "$version" = latest ]; then base=https://github.com/MaxLaneDev/preflight/releases/latest/download
else
 printf '%s\n' "$version" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$' || { echo 'PREFLIGHT_VERSION must be latest or vMAJOR.MINOR.PATCH.' >&2; exit 1; }
 base=https://github.com/MaxLaneDev/preflight/releases/download/$version
fi
umask 077
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' 0
trap 'exit 1' HUP INT TERM
fetch() { curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --connect-timeout 10 --max-time 180 --max-filesize "$3" "$1" -o "$2"; }
fetch "$base/SHA256SUMS" "$temporary/SHA256SUMS" 1048576
awk -v platform="$os-$arch" '
 length($1)!=64 || $1~/[^0-9a-f]/ || $0!~ /^[0-9a-f]+  [A-Za-z0-9_.-]+$/ || seen[$2]++ {bad=1; next}
 $2 ~ ("^preflight-[0-9]+\\.[0-9]+\\.[0-9]+-" platform "\\.tar\\.gz$") {name=$2; digest=$1; count++}
 END {if(bad || count!=1) exit 1; print name; print digest}
' "$temporary/SHA256SUMS" > "$temporary/selected" || { echo 'Invalid, duplicate or missing release checksum.' >&2; exit 1; }
archive=$(sed -n '1p' "$temporary/selected")
expected=$(sed -n '2p' "$temporary/selected")
fetch "$base/$archive" "$temporary/archive.tar.gz" 536870912
if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$temporary/archive.tar.gz" | cut -d ' ' -f 1)
else actual=$(shasum -a 256 "$temporary/archive.tar.gz" | cut -d ' ' -f 1); fi
[ "$actual" = "$expected" ] || { echo 'Archive checksum mismatch.' >&2; exit 1; }
# Stream one trusted executable; archive paths never become filesystem destinations here.
tar -xOzf "$temporary/archive.tar.gz" preflight > "$temporary/preflight"
[ -s "$temporary/preflight" ] || { echo 'Release binary missing.' >&2; exit 1; }
chmod 700 "$temporary/preflight"
"$temporary/preflight" package install "$temporary/archive.tar.gz" --sha256 "$expected" \
 --prefix "${PREFLIGHT_PREFIX:-$HOME/.local/share/preflight/bin}" \
 --config "${PREFLIGHT_CONFIG:-$HOME/.config/preflight/preflight.toml}" \
 --data "${PREFLIGHT_DATA:-$HOME/.local/share/preflight/data}"

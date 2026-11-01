#!/bin/sh
# Offline bootstrap tests: the payload is a harmless shell fixture, never a service.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
. "$root/scripts/lib/common.sh"
require_jq
test_shell=${PREFLIGHT_TEST_SHELL:-sh}
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' 0; trap 'exit 1' HUP INT TERM
mkdir "$tmp/bin" "$tmp/assets" "$tmp/payload"
cat > "$tmp/payload/preflight" <<'PAYLOAD'
#!/bin/sh
jq -n --args '$ARGS.positional' -- "$@"
PAYLOAD
tar -czf "$tmp/assets/preflight-0.1.0-macos-aarch64.tar.gz" -C "$tmp/payload" preflight
sha=$(sha256 "$tmp/assets/preflight-0.1.0-macos-aarch64.tar.gz")
printf '%s  preflight-0.1.0-macos-aarch64.tar.gz\n' "$sha" > "$tmp/assets/SHA256SUMS"
cat > "$tmp/bin/uname" <<'UNAME'
#!/bin/sh
case "$1" in -s) echo "${TEST_OS:-Darwin}";; -m) echo "${TEST_ARCH:-arm64}";; esac
UNAME
cat > "$tmp/bin/curl" <<'CURL'
#!/bin/sh
for arg do case "$arg" in https://*) name=${arg##*/};; esac; done
while [ "$1" != -o ]; do shift; done
cp "$ASSETS/$name" "$2"
CURL
chmod +x "$tmp/bin/"*
export ASSETS="$tmp/assets" PREFLIGHT_PREFIX="$tmp/prefix with spaces" PREFLIGHT_CONFIG="$tmp/config with spaces.toml"
PATH="$tmp/bin:$PATH" "$test_shell" "$root/install.sh" > "$tmp/result"
jq -e --arg prefix "$PREFLIGHT_PREFIX" '.[0:2]==["package","install"] and (index("--prefix") as $i|.[$i+1]==$prefix)' "$tmp/result" >/dev/null
for version in 'v1;touch bad' https://elsewhere; do if PATH="$tmp/bin:$PATH" PREFLIGHT_VERSION="$version" "$test_shell" "$root/install.sh" > "$tmp/result" 2> "$tmp/error"; then exit 1; fi; [ ! -s "$tmp/result" ]; done
cp "$tmp/assets/preflight-0.1.0-macos-aarch64.tar.gz" "$tmp/assets/preflight-0.1.0-macos-x86_64.tar.gz"
printf '%s  preflight-0.1.0-macos-x86_64.tar.gz\n' "$sha" > "$tmp/assets/SHA256SUMS"
PATH="$tmp/bin:$PATH" TEST_ARCH=x86_64 "$test_shell" "$root/install.sh" > "$tmp/result"
jq -e '.[0:2]==["package","install"]' "$tmp/result" >/dev/null
cp "$tmp/assets/preflight-0.1.0-macos-aarch64.tar.gz" "$tmp/assets/preflight-0.1.0-linux-x86_64.tar.gz"
printf '%s  preflight-0.1.0-linux-x86_64.tar.gz\n' "$sha" > "$tmp/assets/SHA256SUMS"
PATH="$tmp/bin:$PATH" TEST_OS=Linux TEST_ARCH=x86_64 "$test_shell" "$root/install.sh" > "$tmp/result"
jq -e '.[0:2]==["package","install"]' "$tmp/result" >/dev/null
printf '%064d  preflight-0.1.0-macos-aarch64.tar.gz\n' 0 > "$tmp/assets/SHA256SUMS"
if PATH="$tmp/bin:$PATH" "$test_shell" "$root/install.sh" > "$tmp/result" 2> "$tmp/error"; then exit 1; fi
[ ! -s "$tmp/result" ]; grep -i 'checksum mismatch' "$tmp/error" >/dev/null
echo 'Shell bootstrap checks passed'

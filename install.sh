#!/bin/sh
# Install the released server, Web UI and MCP without starting or stopping services.
set -eu
command -v python3 >/dev/null 2>&1 || { echo 'Python 3.9 or later is required for installation.' >&2; exit 1; }
python3 -c 'import sys; sys.exit(sys.version_info < (3, 9))' || { echo 'Python 3.9 or later is required for installation.' >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { echo 'curl is required.' >&2; exit 1; }
case "$(uname -s)" in Darwin) os=macos ;; Linux) os=linux ;; *) echo 'This release supports macOS and Linux ARM64.' >&2; exit 1 ;; esac
case "$(uname -m)" in arm64|aarch64) arch=aarch64 ;; *) echo 'Only ARM64 packages are currently released. x86_64 is deferred.' >&2; exit 1 ;; esac
version=${PREFLIGHT_VERSION:-latest}
if [ "$version" = latest ]; then
 base=https://github.com/MaxLaneDev/preflight/releases/latest/download
else
 python3 -c 'import re,sys;sys.exit(not bool(re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+",sys.argv[1])))' "$version" || { echo 'PREFLIGHT_VERSION must be latest or vMAJOR.MINOR.PATCH.' >&2; exit 1; }
 base=https://github.com/MaxLaneDev/preflight/releases/download/$version
fi
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
fetch() { curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --connect-timeout 10 --max-time 180 --max-filesize 1048576 "$1" -o "$2"; }
fetch "$base/SHA256SUMS" "$temporary/SHA256SUMS"
fetch "$base/install-preflight.py" "$temporary/install-preflight.py"
python3 - "$temporary" "$os" "$arch" <<'PY'
import hashlib,pathlib,re,sys
root=pathlib.Path(sys.argv[1]); entries={}
for line in (root/'SHA256SUMS').read_text().splitlines():
 match=re.fullmatch(r'([0-9a-f]{64})  ([A-Za-z0-9_.-]+)',line)
 if not match:raise SystemExit('Invalid release checksum file')
 digest,name=match.groups()
 if name in entries:raise SystemExit('Duplicate release checksum')
 entries[name]=digest
if hashlib.sha256((root/'install-preflight.py').read_bytes()).hexdigest()!=entries.get('install-preflight.py'):raise SystemExit('Installer checksum mismatch')
archives=[name for name in entries if re.fullmatch(r'preflight-[0-9]+\.[0-9]+\.[0-9]+-'+sys.argv[2]+'-'+sys.argv[3]+r'\.tar\.gz',name)]
if len(archives)!=1:raise SystemExit('No unique package for this platform')
(root/'archive').write_text(archives[0]);(root/'digest').write_text(entries[archives[0]])
PY
archive=$(cat "$temporary/archive")
sha256=$(cat "$temporary/digest")
python3 "$temporary/install-preflight.py" install "$base/$archive" --sha256 "$sha256" \
 --prefix "${PREFLIGHT_PREFIX:-$HOME/.local/share/preflight/bin}" \
 --config "${PREFLIGHT_CONFIG:-$HOME/.config/preflight/preflight.toml}" \
 --data "${PREFLIGHT_DATA:-$HOME/.local/share/preflight/data}"

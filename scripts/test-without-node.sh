#!/bin/sh
# Prove service/release tooling works even when every Node invocation fails.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
packages=${1:?package directory}
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' 0; trap 'exit 1' HUP INT TERM
cat > "$tmp/node" <<'BLOCKED'
#!/bin/sh
echo 'Unexpected Node invocation in service/release tooling.' >&2
exit 97
BLOCKED
chmod +x "$tmp/node"; PATH=$tmp:$PATH; export PATH
sh "$root/scripts/test-client-notices.sh"
sh "$root/scripts/test-install-sh.sh"
sh "$root/scripts/test-codex-plugin.sh"
sh "$root/scripts/test-public-export.sh"
sh "$root/scripts/test-release.sh"
sh "$root/scripts/smoke-package.sh" "$packages"
echo 'Service, package, plugin, export, license and MCP checks passed without Node'

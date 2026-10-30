# Sourced by setup: each job must begin without a previous job's installation.
test ! -e "$PREFLIGHT_WORKSPACE/.tools"
export HOME="$PREFLIGHT_WORKSPACE/.home"
export TMPDIR="$PREFLIGHT_WORKSPACE/.tmp"
export XDG_CACHE_HOME="$PREFLIGHT_WORKSPACE/.cache"
export CARGO_HOME="$PREFLIGHT_WORKSPACE/.cargo"
export RUSTUP_HOME="$PREFLIGHT_WORKSPACE/.rustup"
export PATH="$PREFLIGHT_WORKSPACE/.tools/bin:$PATH"
mkdir -p "$HOME" "$TMPDIR" "$XDG_CACHE_HOME" "$CARGO_HOME" "$RUSTUP_HOME" \
  "$PREFLIGHT_WORKSPACE/.tools/bin"
cat > "$PREFLIGHT_WORKSPACE/.tools/bin/workspace-tool" <<'TOOL'
#!/bin/sh
set -e
for directory in "$HOME" "$TMPDIR" "$XDG_CACHE_HOME" "$CARGO_HOME" "$RUSTUP_HOME"; do
  case "$directory" in "$PREFLIGHT_WORKSPACE"/*) ;; *) exit 9 ;; esac
  test -d "$directory"
  printf 'job-owned\n' > "$directory/installed-marker"
done
printf '%s\n' "$PREFLIGHT_WORKSPACE" > installation.txt
TOOL
chmod +x "$PREFLIGHT_WORKSPACE/.tools/bin/workspace-tool"

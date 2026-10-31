#!/bin/sh
# A release's verified server binary owns package validation and activation.
set -eu
operation=${1:?Usage: install-preflight.sh install ARCHIVE --sha256 SHA --prefix DIR --config FILE | status|rollback|uninstall --prefix DIR}
if [ "$operation" = install ]; then
 source=${2:?archive path or HTTPS URL}; shift 2
 expected=''
 previous=''
 for argument do
  if [ "$previous" = --sha256 ]; then [ -z "$expected" ] || exit 1; expected=$argument; fi
  previous=$argument
 done
 [ "${#expected}" = 64 ] || { echo 'Expected SHA-256 required.' >&2; exit 1; }
 case "$expected" in *[!0-9a-f]*) echo 'Invalid SHA-256.' >&2; exit 1;; esac
 umask 077; tmp=$(mktemp -d)
 trap 'rm -rf "$tmp"' 0; trap 'exit 1' HUP INT TERM
 case "$source" in
  https://*) curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --connect-timeout 10 --max-time 180 --max-filesize 536870912 "$source" -o "$tmp/archive.tar.gz";;
  *://*) echo 'Release downloads require HTTPS.' >&2; exit 1;;
  *) [ -f "$source" ] && [ ! -L "$source" ] || exit 1; [ "$(wc -c < "$source")" -le 536870912 ] || exit 1; cp "$source" "$tmp/archive.tar.gz";;
 esac
 if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$tmp/archive.tar.gz" | cut -d ' ' -f 1); else actual=$(shasum -a 256 "$tmp/archive.tar.gz" | cut -d ' ' -f 1); fi
 [ "$actual" = "$expected" ] || { echo 'Archive checksum mismatch.' >&2; exit 1; }
 tar -xOzf "$tmp/archive.tar.gz" preflight > "$tmp/preflight"
 [ -s "$tmp/preflight" ] || exit 1
 chmod 700 "$tmp/preflight"
 "$tmp/preflight" package install "$tmp/archive.tar.gz" "$@"
else
 prefix=''; previous=''
 for argument do if [ "$previous" = --prefix ]; then prefix=$argument; fi; previous=$argument; done
 if [ -n "${PREFLIGHT_BINARY:-}" ]; then binary=$PREFLIGHT_BINARY
 elif [ -n "$prefix" ] && [ -x "$prefix/current/preflight" ]; then binary=$prefix/current/preflight
 elif command -v preflight >/dev/null 2>&1; then binary=$(command -v preflight)
 else echo 'Set PREFLIGHT_BINARY to a current Preflight executable.' >&2; exit 1; fi
 exec "$binary" package "$@"
fi

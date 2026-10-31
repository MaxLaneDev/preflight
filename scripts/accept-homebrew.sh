#!/bin/sh
# Real local-tap test. Refuse existing installations and leave other formulae alone.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
[ "$(uname -s)" = Darwin ] || { echo 'Requires macOS Homebrew.' >&2; exit 1; }
package=${1:?package JSON}; scenario=homebrew
. "$root/scripts/lib/acceptance.sh"
brew=${PREFLIGHT_BREW:-/opt/homebrew/bin/brew}
export HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_ANALYTICS=1 HOMEBREW_NO_INSTALL_CLEANUP=1 HOMEBREW_NO_ENV_HINTS=1 HOMEBREW_NO_AUTOREMOVE=1
XDG_CONFIG_HOME=$out/xdg; export XDG_CONFIG_HOME
prefix=$("$brew" --prefix); settings=$prefix/etc/preflight; log=$prefix/var/log/preflight.log
source=$out/tap; tap=codex-preflight/acceptance; formula=$tap/preflight; tapdir=$prefix/Library/Taps/codex-preflight/homebrew-acceptance
for item in "$settings" "$log" "$tapdir"; do [ ! -e "$item" ] || { echo 'Owned test paths must be absent.' >&2; exit 1; }; done
if "$brew" list --formula preflight >/dev/null 2>&1; then echo 'Preflight is already installed with Homebrew.' >&2; exit 1; fi
"$brew" list --versions --formula | sort > "$out/inventory-before.txt"
tapped=false; installed=false; service=false
run() { "$brew" "$@" >> "$out/brew.log" 2>&1 || { echo "brew failed; inspect $out/brew.log" >&2; return 1; }; }
finish() {
 if [ "$service" = true ]; then run services stop "$formula"; fi
 if [ "$installed" = true ]; then run uninstall --force "$formula"; fi
 if [ "$tapped" = true ]; then run untap "$tap"; fi
 if [ -d "$settings" ]; then
  for file in "$settings"/* "$settings"/.[!.]*; do [ -e "$file" ] || continue; case "$(basename "$file")" in preflight.toml|admin-token) :;; *) echo 'Unrelated settings files; preserving directory.' >&2; return 1;; esac; done
  rm -f "$settings/preflight.toml" "$settings/admin-token"; rmdir "$settings"
 fi
 rm -f "$log"; cleanup
}
trap finish 0
mkdir -p "$source/Formula"
archive=$(jq -er '.[0].archive' "$package"); archive=$(CDPATH= cd -- "$(dirname "$archive")" && pwd)/$(basename "$archive")
sha=$(jq -er '.[0].sha256' "$package")
"$root/scripts/release.sh" formula "$archive" --sha256 "$sha" --url https://example.invalid/preflight.tar.gz --homepage https://example.invalid/preflight --destination "$source/Formula/preflight.rb" > /dev/null
# The owned tap uses a local file, not an unpublished public URL.
sed "s|https://example.invalid/preflight.tar.gz|file://$archive|" "$source/Formula/preflight.rb" > "$out/local.rb"; mv "$out/local.rb" "$source/Formula/preflight.rb"
git -C "$source" init -q; git -C "$source" add .; git -C "$source" -c user.name='Preflight acceptance' -c user.email=acceptance@example.invalid commit -qm 'Test local formula'
run tap "$tap" "$source"; tapped=true
run trust --formula "$formula"; run install --formula --build-from-source "$formula"; installed=true
run test "$formula"
config=$settings/preflight.toml; credential=$settings/admin-token
key=$(cat "$credential"); [ "${#key}" -ge 32 ]
[ "$(stat -f '%Lp' "$credential")" = 600 ]
grep -F 'admin_token_file' "$config" >/dev/null
if grep -F "$key" "$config"; then exit 1; fi
sed -e "s|127.0.0.1:4170|127.0.0.1:$port|" -e "s|$prefix/var/preflight|$data|" "$config" > "$out/new.toml"
cat "$out/new.toml" > "$config"; cp "$config" "$out/saved.toml"; cp "$credential" "$out/saved-token"
wait_service() { attempts=0; until curl --noproxy '*' -fsS --max-time 1 "$base/health" >/dev/null 2>&1; do attempts=$((attempts+1)); [ "$attempts" -lt 200 ] || return 1; sleep .1; done; }
run services start "$formula"; service=true; wait_service
api server-settings "$(jq -n --arg key "$key" '{admin_token:$key}')" > "$out/settings.json"
jq -e '.admin_token_file_supported' "$out/settings.json" >/dev/null
api capabilities | jq -e '.features.bundled_web_ui' >/dev/null
startup_checked=false
if jq -e '.autostart.available' "$out/settings.json" >/dev/null; then
 jq -e '.autostart.manager=="homebrew" and .autostart.enabled' "$out/settings.json" >/dev/null
 label=$(jq -r '.autostart.label' "$out/settings.json"); target=gui/$(id -u)/$label
 pid_before=$(launchctl print "$target" | awk '/pid =/ {print $3;exit}')
 api server-autostart "$(jq --arg key "$key" '{admin_token:$key,expected_revision:.revision,enabled:false}' "$out/settings.json")" > "$out/disabled.json"
 jq -e '.autostart.enabled==false' "$out/disabled.json" >/dev/null
 api server-autostart "$(jq --arg key "$key" '{admin_token:$key,expected_revision:.revision,enabled:true}' "$out/disabled.json")" | jq -e '.autostart.enabled and .autostart.manager=="homebrew"' >/dev/null
 [ "$(launchctl print "$target" | awk '/pid =/ {print $3;exit}')" = "$pid_before" ]; startup_checked=true
fi
sed '/class Preflight < Formula/a\
  revision 1\
' "$tapdir/Formula/preflight.rb" > "$out/revision.rb"; cat "$out/revision.rb" > "$tapdir/Formula/preflight.rb"
run upgrade "$formula"; cmp "$config" "$out/saved.toml"; cmp "$credential" "$out/saved-token"
run services restart "$formula"; wait_service
api server-settings "$(jq -n --arg key "$key" '{admin_token:$key}')" | jq -e '.admin_token_file_supported' >/dev/null
run services stop "$formula"; service=false
attempts=0; while curl --noproxy '*' -fsS --max-time 1 "$base/health" >/dev/null 2>&1; do attempts=$((attempts+1)); [ "$attempts" -lt 100 ] || exit 1; sleep .1; done
run uninstall --force "$formula"; installed=false
cmp "$config" "$out/saved.toml"; cmp "$credential" "$out/saved-token"; [ -f "$data/preflight.sqlite3" ]
run untap "$tap"; tapped=false
"$brew" list --versions --formula | sort > "$out/inventory-after.txt"; cmp "$out/inventory-before.txt" "$out/inventory-after.txt"
jq -n --argjson startup "$startup_checked" '{actual_homebrew_install:true,web_startup_policy_reuses_homebrew_agent:$startup,formula_test:true,formula_revision_upgrade:true,service_start_restart_stop:true,config_and_credential_preserved:true,uninstall_preserves_data:true,local_file_download:true,public_https_download:"not tested",production_mutations:false}' > "$out/acceptance.json"
echo "$out"

# Releases

Release packages include the server, Web UI and MCP. Public downloads and the
Homebrew tap are pending release acceptance.

## Build and check packages

```sh
cargo build --release --locked --workspace
sh scripts/test-release.sh
sh scripts/test-install-sh.sh
sh scripts/test-codex-plugin.sh
sh scripts/release.sh package --server target/release/preflight --destination dist
sh scripts/smoke-package.sh dist
```

CI checks macOS/Linux ARM64. Windows and x86_64 remain deferred. A release tag must
match the Cargo product version; the workflow creates a draft with checked packages,
installer, plugin ZIP and SHA256SUMS. Publish only after real download/install checks.

## Homebrew

```sh
sh scripts/release.sh formula ARCHIVE --sha256 SHA256 \
  --url HTTPS_RELEASE_ASSET_URL --homepage HTTPS_REPOSITORY_URL \
  --destination preflight.rb
```

Use actual accepted asset URLs/digests. Ruby syntax alone does not verify installation.
Publishing a stable release triggers `homebrew.yml`: it verifies the archive checksum
and updates `Formula/preflight.rb` in the same repository. Older release events cannot
replace the latest formula. [Install commands](installation.md).

## Public Codex plugin

Manual packages include MCP, the operating skill and optional reviewed hooks.
Public directory publication needs a separate eligible package and OpenAI review.
[Plugin packaging](codex-plugin.md).

## Server release notifications

```toml
release_repository = "owner/repository"
```

Omit it for no network checks. The server checks public stable GitHub releases every
six hours without a token. The update icon opens a plaintext changelog. Product
SemVer is separate from MCP protocol. `server_release_status` reads compact status;
`details: true` includes notes. Admin-only `server_release_check` is throttled to
once per minute. No binaries are automatically installed and no runs are stopped.

## Script dependencies

Installation and Codex hooks need no Python or Node.js. Package verification,
activation and rollback run in the Rust CLI.

Development commands use `sh`, Node.js 22, `jq`, `git` and `unzip`; archive
acceptance also uses the `sqlite3` CLI. Browser checks retain their Node.js runner.
macOS service and disk-image checks require a real user session.

```sh
PREFLIGHT_BINARY="$PWD/target/release/preflight" sh scripts/test-install-sh.sh
sh scripts/test-release.sh
sh scripts/test-codex-plugin.sh
sh scripts/test-public-export.sh
sh scripts/accept-environment.sh
```

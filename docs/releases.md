# Releases

Release packages include the server, Web UI and MCP. Download
[the latest release](https://github.com/MaxLaneDev/preflight/releases/latest) or use
the [installer and Homebrew tap](installation.md).

## Build and check packages

```sh
cargo build --release --locked --workspace
sh scripts/test-release.sh
sh scripts/test-install-sh.sh
sh scripts/test-codex-plugin.sh
sh scripts/release.sh package --server target/release/preflight --destination dist
sh scripts/smoke-package.sh dist
```

The 0.1.2 candidate matrix checks macOS ARM64/x86_64, Linux ARM64/x86_64 and
Windows x86_64. A release tag must
match the Cargo product version; the workflow creates a draft with checked packages,
installer, plugin ZIP and SHA256SUMS. Publish only after real download/install checks.
Windows archives use the static C runtime and include the same Web UI and MCP;
`install.ps1` verifies the checksum and installs without administrator access.
Native Windows acceptance checks install/reinstall, UI/API/MCP startup, refusal
to uninstall a running server, and removal that preserves configuration and data.
After publication, `install-release.yml` downloads the published installers and
checks their archive selection and UI/MCP startup on all five native targets.

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

Service/release checks use shell, `curl`, `jq`, `git` and `unzip`. Archive checks
also use the `sqlite3` CLI. Browser tests and editor rebuilds use Node.js 26.13.0;
macOS service checks require a user session.

```sh
sh scripts/release.sh package --server target/release/preflight --destination dist
sh scripts/test-without-node.sh dist
```

The second command blocks Node execution while checking licenses, bootstrap,
plugin packaging, source export, release collection, server startup and MCP.

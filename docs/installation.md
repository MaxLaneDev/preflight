# Installation

Public packages and the Homebrew tap are not published. Use [server setup](server-setup.md)
for a source installation.

After the first public release, install the server with either command:

```sh
curl -fsSL https://github.com/MaxLaneDev/preflight/releases/latest/download/install.sh | sh
```

```sh
brew tap MaxLaneDev/preflight https://github.com/MaxLaneDev/preflight
brew install MaxLaneDev/preflight/preflight
brew services start MaxLaneDev/preflight/preflight
```

The first release targets macOS ARM64 and Linux ARM64; the Homebrew formula targets
macOS ARM64. The installer needs curl, tar and a SHA-256 tool; it does not start the server.

## Install a local release package

```sh
sh scripts/release.sh package --server target/release/preflight --destination dist
sh dist/install-preflight.sh install dist/ARCHIVE.tar.gz \
  --sha256 ARCHIVE_SHA256 --prefix /path/to/binaries \
  --config /path/to/config/preflight.toml --data /path/to/data
```

Use the generated archive name and its `.sha256` digest. No Python runtime is required. Configuration and data must be outside the
binary directory. The installer creates a private admin-token file for a new config.
It prints the start command and never restarts a running service.

## Update, roll back, remove

Run `install` with a new archive and checksum to update. Restart after active work finishes.

```sh
sh install-preflight.sh status --prefix /path/to/binaries
sh install-preflight.sh rollback --prefix /path/to/binaries
sh install-preflight.sh uninstall --prefix /path/to/binaries
```

Stop the service before uninstalling. Configuration and data remain on disk.
Binary rollback does not reverse database migrations. [Back up first](server-backup.md).

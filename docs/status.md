# Status

| Area | Checked |
| --- | --- |
| Local engine, MCP/HTTP, cache, retention, archives | Rust regression suite |
| Crate updates | Rust 1.89 compilation; database, backup and age compatibility |
| Bundled UI and compatibility client | Headless Firefox in both modes, 390 px mobile navigation; 47 frontend tests |
| SSH and Docker workers | 14 opt-in checks with loopback OpenSSH and Linux ARM64 Docker; v0.1.3 Linux x86_64 SSH acceptance under Docker emulation |
| Worker service | macOS launchd restart, identity retention and state-lock release |
| Crash recovery | Owned-worker SIGKILL and explicit journal recovery |
| Disk pressure | ENOSPC recovery on an owned 64 MiB HFS+ image; scoped watermark cleanup |
| Shell scripts / distribution | Shell/Rust install, release and MCP checks pass with Node blocked; browser tests retain Node |
| Public releases / Homebrew | v0.1.3 published; five native targets, six published installer checks, macOS ARM64 HTTPS/Homebrew install and upgrade checked |
| Plugin distribution | GitHub release packages; public OpenAI directory publication cancelled by the author |

[Native build checks](https://github.com/MaxLaneDev/preflight/actions/runs/41440287016)
and [published installer checks](https://github.com/MaxLaneDev/preflight/actions/runs/41445920481) passed.
macOS 27 ARM64 passed both the native compatibility checks and installation of the
published Mac ARM64 archive on the `xcode-27` runner.
The published v0.1.3 Linux x86_64 binary also passed an SSH worker check in an
amd64 Ubuntu container on an ARM64 Docker host: strict host-key rejection,
installation/enrollment, C compilation, x86_64 ELF artifact transfer, JUnit,
parent/child cancellation, gateway loss, explicit journal recovery and a fresh
successful run with the same worker identity. The lost job was not replayed.
This was CPU emulation; a physical remote x86_64 host remains untested and deferred.
NAT traversal remains future work. Local check receipts stay outside the public tree.

```sh
cargo test --release --locked --workspace
node --test crates/preflight-client/tests/*.test.mjs
sh scripts/test-release.sh
```

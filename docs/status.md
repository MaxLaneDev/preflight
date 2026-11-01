# Status

| Area | Checked |
| --- | --- |
| Local engine, MCP/HTTP, cache, retention, archives | Rust regression suite |
| Crate updates | Rust 1.89 compilation; database, backup and age compatibility |
| Bundled UI and compatibility client | Headless Firefox in both modes, 390 px mobile navigation; 40 frontend tests |
| SSH and Docker workers | 14 opt-in checks with loopback OpenSSH and Linux ARM64 Docker |
| Worker service | macOS launchd restart, identity retention and state-lock release |
| Crash recovery | Owned-worker SIGKILL and explicit journal recovery |
| Disk pressure | ENOSPC recovery on an owned 64 MiB HFS+ image; scoped watermark cleanup |
| Shell scripts / distribution | Shell/Rust install, release and MCP checks pass with Node blocked; browser tests retain Node |
| Public releases / Homebrew | v0.1.1 published; ARM64 CI, macOS curl/Homebrew installation and UI/MCP startup checked |
| Public plugin directory | Not submitted; eligibility/review pending |

Linux x86_64 and Windows x86_64 native CI/release checks are in progress for 0.1.2.
macOS 27 ARM64 has a separate compatibility check on the `xcode-27` runner;
support is not accepted until its tests and the published Mac ARM64 installer pass.
Physical remote x86_64 worker acceptance remains deferred. ARM64 Docker is not x86_64 acceptance.
NAT traversal remains future work. Local check receipts stay outside the public tree.

```sh
cargo test --release --locked --workspace
node --test crates/preflight-client/tests/*.test.mjs
sh scripts/test-release.sh
```

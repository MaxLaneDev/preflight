# Status

| Area | Checked |
| --- | --- |
| Local engine, MCP/HTTP, cache, retention, archives | Rust regression suite |
| Crate updates | Rust 1.89 compilation; database, backup and age compatibility |
| Bundled UI and compatibility client | Browser acceptance, including mobile viewports |
| SSH and Docker workers | Same-machine real transport/container tests |
| Crash recovery | Owned-worker SIGKILL and explicit journal recovery |
| Shell scripts / distribution | Python-free bootstrap and hooks; Rust package verification and rollback |
| Public releases / Homebrew | Not published |
| Public plugin directory | Not submitted; eligibility/review pending |

Windows and physical x86_64 are deferred. ARM64 Docker is not x86_64 acceptance.
NAT traversal remains future work. Local check receipts stay outside the public tree.

```sh
cargo test --release --locked --workspace
node --test crates/preflight-client/tests/*.test.mjs
sh scripts/test-release.sh
```

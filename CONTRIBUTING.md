# Contributing

Use Rust stable and a C compiler. Shell checks need `jq`, `curl`, `git` and `unzip`.
Browser tests and editor development use Node.js 26.13.0. Installed releases need
neither Node.js nor Python.

```sh
cargo fmt --all --check
cargo clippy --release --locked --workspace --all-targets -- -D warnings
cargo test --release --locked --workspace
cargo build --release --locked --workspace
node --test crates/preflight-client/tests/*.test.mjs
sh scripts/test-release.sh
```

Tests that start servers need loopback networking. Use disposable project/data
folders. Do not test cancellation, cleanup or recovery against someone's active
server. Some platform/infrastructure tests are ignored until that environment is
provided; an ignored test is not a pass.

For browser acceptance, build both release binaries and use an installed Playwright
module and Firefox executable:

```sh
PF_PLAYWRIGHT_MODULE=/absolute/path/to/playwright \
PF_FIREFOX_EXECUTABLE=/absolute/path/to/firefox \
sh scripts/test-client.sh --bundled
```

Omit `--bundled` to check the standalone compatibility client. Each run creates
temporary projects, a private settings credential and its own server processes.
Evidence goes under `.validation/client-*`; the browser and owned processes close
on completion or failure. The suite checks real builds, log history scrolling,
source diffs, test sources, revisions, schedules, experiments and mobile navigation.
It never uses the working server's database. Runtime-specific Docker and SSH
acceptance remains separate.

For real worker tests, select an already cached Linux ARM64 image. These tests
start private loopback SSH daemons with temporary keys and remove their own
containers. They do not change system SSH configuration.

```sh
PREFLIGHT_TEST_DOCKER_IMAGE=sha256:YOUR_CACHED_IMAGE_DIGEST \
cargo test --release --locked --test containers --test remote_containers \
  --test worker_connections -- --ignored --test-threads=1
```

On macOS, test a temporary launchd worker agent:

```sh
cargo test --release --locked --test worker_service -- --ignored --test-threads=1
```

On macOS, test full-disk recovery on an owned 64 MiB image and scoped cleanup
under a free-space watermark. The script detaches and removes its image afterward.

```sh
sh scripts/accept-storage-volume.sh
```

Keep engine behavior shared by MCP and HTTP. Add regression coverage where a change
can lose data, weaken permissions or change a protocol contract. Store new project
configuration in `.preflight/`, never in a language-specific global mode.

User-facing text and documentation are English. Describe the behavior and its
limits; leave internal plans, chat transcripts, temporary logs and generated build
outputs out of patches. Update the relevant guide when a command or setting changes.

Contributions use the repository's MIT license. Keep third-party license notices
when adding or updating dependencies. See [licensing](docs/licensing.md).

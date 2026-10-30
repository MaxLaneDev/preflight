# Contributing

Use Rust stable, a C compiler and Python 3.9+. Node.js 22 is needed for client tests
and editor development, not for running a release.

```sh
cargo fmt --all --check
cargo clippy --release --locked --workspace --all-targets -- -D warnings
cargo test --release --locked --workspace
cargo build --release --locked --workspace
node --test crates/preflight-client/tests/*.test.mjs
python3 scripts/test-release.py
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
python3 scripts/test-client.py --bundled
```

Omit `--bundled` to check the standalone compatibility client. Each run creates
temporary projects, a private settings credential and its own server processes.
Evidence goes under `.validation/client-*`; the browser and owned processes close
on completion or failure. The suite checks real builds, log history scrolling,
source diffs, test sources, revisions, schedules, experiments and mobile navigation.
It never uses the working server's database. Runtime-specific Docker and SSH
acceptance remains separate.

Keep engine behavior shared by MCP and HTTP. Add regression coverage where a change
can lose data, weaken permissions or change a protocol contract. Store new project
configuration in `.preflight/`, never in a language-specific global mode.

User-facing text and documentation are English. Describe the behavior and its
limits; leave internal plans, chat transcripts, temporary logs and generated build
outputs out of patches. Update the relevant guide when a command or setting changes.

Contributions use the repository's MIT license. Keep third-party license notices
when adding or updating dependencies. See [licensing](docs/licensing.md).

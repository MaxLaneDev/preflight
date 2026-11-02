# Prepare tools

Add `setup` to the consuming stage. It runs before the main command, on the same
worker or inside the same container, under one timeout.

```yaml
id: tests
setup:
  - program: .
    args: [./scripts/prepare.sh]
command:
  program: test-runner
  args: [tests]
  timeout_seconds: 600
```

A sourced `prepare.sh` can install a checked tool archive under the workspace:

```sh
export HOME="$PREFLIGHT_WORKSPACE/.home"
export TMPDIR="$PREFLIGHT_WORKSPACE/.tmp"
export PATH="$PREFLIGHT_WORKSPACE/.tools/bin:$PATH"
mkdir -p "$HOME" "$TMPDIR" "$PREFLIGHT_WORKSPACE/.tools/bin"
# Verify the archive checksum, then unpack into .tools.
```

`setup` targets a POSIX environment: native macOS/Linux workers or Linux
containers. A Windows server can dispatch it to those targets. Native Windows
commands run normally, but the POSIX `setup` field is rejected for Windows workers.

Use `.` to preserve exports; ordinary setup commands are subprocesses. A failed
step prevents the main command. Setup requires POSIX `sh`, accepts at most 16
commands, and cannot be combined with build-cache reuse, pinned profiles or fixture adapters.

| Need | Use |
| --- | --- |
| Disposable system packages | Prepared container image |
| Job-local tools | Workspace installation prefix in sourced setup |
| Shared compiler across container jobs | Immutable image on the selected worker's daemon |
| A build result in another stage | Declared artifact/module input |

Native workspace prefixes are conventions, not filesystem isolation. Global
`apt`/`brew` installations change the host and are not automatically uninstalled.
A native prepare stage cannot change an unrelated worker's environment.

A [Docker stage](remote-containers.md) gets a private container. Downloads need
explicit network access; writing system directories needs a writable root and
appropriate container user. Image layers/caches have separate retention.

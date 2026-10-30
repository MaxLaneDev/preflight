<img src="assets/branding/preflight-icon.png" width="96" height="96" alt="Preflight">

# Preflight

Run builds and tests locally. Inspect logs, diffs and artifacts in the Web UI or
through MCP. One Rust server stores results in SQLite and files on disk.

## Start

Requires Rust 1.89 or newer and a C compiler for SQLite.

```sh
cargo build --release --locked -p preflight
cp -n preflight.example.toml preflight.toml
./target/release/preflight --config preflight.toml serve
```

Web: **http://127.0.0.1:4170** · MCP: **http://127.0.0.1:4170/mcp**

[Configure the server](docs/server-setup.md): admin token, projects, limits and autostart.
Public curl/Homebrew packages are not published.

## Add a pipeline

Save as `.preflight/pipeline.yaml` in your project. Add `/.preflight/` to `.gitignore`.

```yaml
version: 1
pipeline:
  stages:
    - id: test
      kind: test
      command:
        program: go
        args: [test, ./...]
```

```sh
preflight --config /path/to/preflight.toml test /path/to/project
```

Use the built binary's full path if `preflight` is not on PATH.
Commands run with the server account's permissions. Run trusted projects only.

## Examples

| Project | Configuration |
| --- | --- |
| Go | [go-basic](examples/go-basic/preflight.yaml) |
| C | [c-basic](examples/c-basic/preflight.yaml) |
| C++ / CMake | [cpp-basic](examples/cpp-basic/preflight.yaml) |

## Guides

- [Server setup](docs/server-setup.md) · [MCP](docs/mcp-configuration.md)
- [Pipelines](docs/pipelines.md) · [Experiments](docs/experiments.md) · [Schedules](docs/schedules.md)
- [SSH workers](docs/worker-bootstrap.md) · [Environment setup](docs/environment-setup.md)
- [Logs and search](docs/evidence.md) · [Cleanup](docs/storage-gc.md) · [Archives](docs/run-archives.md)
- [All guides](docs/README.md)

## Next

Public releases, curl/Homebrew distribution, additional platforms and workers that
connect through NAT without inbound ports. Windows and physical x86_64 checks are deferred.

[MIT license](LICENSE). Packages include dependency notices.

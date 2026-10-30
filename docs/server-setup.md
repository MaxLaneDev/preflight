# Set up the server

## 1. Build and create an admin token

For a new installation, run from the source directory. Keep an existing token and configuration.

```sh
cargo build --release --locked -p preflight
PREFLIGHT_BIN="$(pwd)/target/release/preflight"
PREFLIGHT_CONFIG_DIR="$HOME/.config/preflight"
umask 077
mkdir -p "$PREFLIGHT_CONFIG_DIR"
"$PREFLIGHT_BIN" credential generate > "$PREFLIGHT_CONFIG_DIR/admin-token"
```

## 2. Write the configuration

Save this as `~/.config/preflight/preflight.toml`:

```toml
version = 1
bind = "127.0.0.1:4170"
data_dir = "data"
admin_token_file = "admin-token"
allowed_roots = []
max_parallel_commands = 4
max_active_runs = 8
```

Relative paths resolve beside the configuration file. `data` stores the database,
logs, workspaces and artifacts. An empty `allowed_roots` denies project registration.

## 3. Start

```sh
"$PREFLIGHT_BIN" --config "$PREFLIGHT_CONFIG_DIR/preflight.toml" serve
```

Leave the server running. In another terminal, check it:

```sh
curl -fsS http://127.0.0.1:4170/health
```

Open **http://127.0.0.1:4170 → Settings**. Unlock with the token from
`~/.config/preflight/admin-token`. On macOS, copy it without printing it:

```sh
pbcopy < "$PREFLIGHT_CONFIG_DIR/admin-token"
```

## 4. Allow a project

In Settings, add the absolute project directory to **Allowed project roots** and
save. Register that directory in the UI or [through MCP](mcp-configuration.md).

To configure roots before starting, replace the empty list:

```toml
allowed_roots = ["/absolute/path/to/project"]
```

The directory must exist. Keep the server on loopback; commands run with its user's permissions.

## 5. Set resource limits

Settings applies these changes live:

```toml
max_parallel_commands = 4
max_active_runs = 8
memory_budget_mib = 8192
default_stage_memory_mib = 512
max_storage_mib = 32768
min_free_disk_bytes = 5368709120
```

| Setting | Meaning |
| --- | --- |
| Command slots | Shared stage capacity; a stage can reserve several slots |
| Concurrent runs | Maximum admitted runs across projects |
| Memory budget | Scheduling budget in MiB; native processes are not memory-sandboxed |
| Maximum storage | Admission threshold for stored file bytes in MiB |
| Free disk floor | Minimum free bytes required to admit new work; above: 5 GiB |

Storage thresholds do not interrupt running commands or impose filesystem quotas.
Use [cleanup policies](storage-policies.md) to reclaim old payloads.

## 6. Connect MCP and enable startup

MCP endpoint: `http://127.0.0.1:4170/mcp`. [Client configuration](mcp-configuration.md).

On macOS: **Settings → Start Preflight at login**. It applies at the next login.
For Linux, [export a systemd user service](services.md).

Listener addresses and `data_dir` require a restart after current runs finish.
Other supported settings hot-reload. See [SSH setup](worker-bootstrap.md) for remote workers.

# Server settings

Start with [server setup](server-setup.md). Open **Settings** in the header and
unlock with the administrator token. Appearance controls remain in the header.

## Read and save through MCP

`server_settings`

<!-- mcp: server_settings -->
```json
{
  "admin_token": "ADMIN_TOKEN"
}
```

Copy the returned `saved` configuration, edit the fields, and send it as
`configuration` with the returned `expected_revision`. Saves replace the complete
configuration; stale revisions fail. Keep the token out of logs and browser storage.

## Useful limits

```toml
max_parallel_commands = 4
max_active_runs = 8
memory_budget_mib = 8192
default_stage_memory_mib = 512
max_storage_mib = 32768
min_free_disk_bytes = 5368709120
```

Memory values use MiB. Disk-free floor uses bytes. These are admission limits;
native processes are not memory-sandboxed and running work is not killed.

Supported changes apply live. `version`, `bind`, `worker_bind` and `data_dir` are
startup-only. Restart after current work finishes to change them.

Optional fields: `docker_command`, `ssh_command`, `worker_launcher`, `codex_command`,
`release_repository` and [storage policies](storage-policies.md). Executable paths
must be absolute. Empty optional fields disable their feature.

On macOS, **Start Preflight at login** controls future login startup without
stopping the current server. [Service exports](services.md) cover Linux.

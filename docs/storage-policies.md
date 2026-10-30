# Automatic cleanup

Open **Settings → Automatic storage cleanup → Add policy**. Select the project,
payload category, TTL and batch size. Save to apply live.

```toml
[[storage_policies]]
name = "old-stages"
project_id = "PROJECT_ID"
category = "stage_workspaces"
ttl_seconds = 86400
interval_seconds = 3600
batch_limit = 20
```

This expires eligible stage workspaces older than one day, checking hourly.
Pins, modules, active/abandoned operations and retained consumers protect data.
History remains recorded.

For size-based cleanup, add:

```toml
[storage_policies.quota]
high_bytes = 1073741824
low_bytes = 536870912
```

Start above 1 GiB and continue toward 512 MiB. Alternatively, use `disk_pressure`
with `start_below_bytes`/`stop_at_bytes` free-space thresholds; do not combine both
triggers in one policy. A protected or incomplete scope may prevent reaching the target.
Inspect `storage_policy_status` for results. [Manual cleanup](storage-gc.md).

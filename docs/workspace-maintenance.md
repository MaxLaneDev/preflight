# Workspace maintenance

Inspect usage before cleanup:

`storage_usage`

<!-- mcp: storage_usage -->
```json
{
  "project_id": "PROJECT_ID",
  "category": "stage_workspaces",
  "limit": 20
}
```

Use the returned cursor for more rows. Categories include workspaces, artifacts,
module copies, source snapshots, logs and cached outputs. Bounded/incomplete
measurements are marked; never treat them as complete disk totals.

| Need | Operation |
| --- | --- |
| Stop a run | `pipeline_cancel` |
| Invalidate cache entries | `cache_cleanup_preview` → `cache_cleanup_execute` |
| Reclaim stage workspaces | `storage_gc_preview` → `storage_gc_execute` |
| Expire artifact bytes | `artifact_expiry_preview` → `storage_gc_execute` |
| Keep a portable copy | [Run archive](run-archives.md) |
| Hide/remove an object recoverably | Management → Trash |

Preview and execute use the selected project/pipeline scope. Active operations and
retention references are rechecked. [Policies](storage-policies.md) automate this.

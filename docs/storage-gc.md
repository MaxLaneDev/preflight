# Clean up stored data

In the UI, open workspace or pipeline management. Preview the selected scope
before removing payloads.

`storage_gc_preview`

<!-- mcp: storage_gc_preview -->
```json
{
  "project_id": "PROJECT_ID",
  "limit": 20
}
```

After reviewing the returned plan:

`storage_gc_execute`

<!-- mcp: storage_gc_execute -->
```json
{
  "project_id": "PROJECT_ID",
  "plan_id": "PLAN_ID",
  "request_id": "cleanup-1",
  "actor": "user",
  "reason": "Remove completed stage workspaces"
}
```

For output artifacts, request a separate preview:

`artifact_expiry_preview`

<!-- mcp: artifact_expiry_preview -->
```json
{
  "project_id": "PROJECT_ID",
  "finished_before": 1767225600000,
  "expire_artifacts": true,
  "limit": 20
}
```

The example cutoff is 2026-01-01 UTC. Choose your own past retention cutoff.
Plans freeze scope and recheck leases, changed files, module/experiment/cache
references and pins at execution. Exact request replay returns the stored result.
Protected items require a fresh preview after protection is released.

Workspace cleanup preserves logs, results and published artifacts. Artifact expiry
removes output bytes but keeps history/hash metadata. [Cache invalidation](build-cache.md)
deletes no bytes; cache-output eviction has its own preview. Never delete data-dir
files by hand while the server is using them.

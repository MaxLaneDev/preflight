# Worker jobs

`runs_worker_jobs`

<!-- mcp: runs_worker_jobs -->
```json
{
  "run_id": "RUN_ID"
}
```

Inspect worker ID, claim state, output-tree digest, acknowledged log cursor and
truncation. Leased jobs receive verified inputs; declared results return before
workspace removal. Artifacts still pass the engine's normal collection checks.

Cancellation, expired leases and transport loss stop execution and reject late
results. Commands are never automatically replayed. Uncertain cleanup keeps
protection and makes the worker unavailable until recovery.

`worker_recovery_status`

<!-- mcp: worker_recovery_status -->
```json
{
  "project_id": "PROJECT_ID",
  "job_id": "JOB_ID"
}
```

[Recovery](operation-recovery.md) requires verified process termination. A terminal
server run alone is not proof that remote descendants stopped.

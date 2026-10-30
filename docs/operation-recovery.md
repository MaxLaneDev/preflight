# Resolve an abandoned operation

`operations_abandoned`

<!-- mcp: operations_abandoned -->
```json
{
  "project_id": "PROJECT_ID"
}
```

`runs_operations`

<!-- mcp: runs_operations -->
```json
{
  "run_id": "RUN_ID"
}
```

Before resolving, verify that the owning process **and descendants** stopped.
A timeout, stale heartbeat or terminal run does not prove this.

Then use `operation_resolve` with the selected lease, project, request ID, actor,
reason and `process_stopped: true`. Active/referenced running work is rejected.
Only that lease is released; no files are deleted. Repeating the same request
returns its audit result.

For a worker journal:

```sh
preflight worker journal /private/worker-state
preflight worker recover http://127.0.0.1:4171 /private/worker-state JOB_ID --process-stopped
```

Use the assertion only after verification. Recovery retains evidence and never
replays the interrupted command. Create a fresh cleanup preview afterward.

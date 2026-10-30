# Logs

The Logs tab starts at the tail and loads older windows when you scroll upward.
Use wrap, follow-output and jump-to-end controls; manual scrolling pauses following.

`logs_read`

<!-- mcp: logs_read -->
```json
{
  "run_id": "RUN_ID",
  "stage": "build",
  "after": 0
}
```

Pass the returned cursor as `after`. Keep pages bounded; read only the stage you
need. Truncation/expiry is explicit. Lifecycle messages and stdout/stderr retain
their stream identity.

`logs_storage`

<!-- mcp: logs_storage -->
```json
{
  "project_id": "PROJECT_ID",
  "run_id": "RUN_ID"
}
```

Log compression preserves event identity/cursors. Physical SQLite file shrinkage
is separate from payload expiry. [Search](evidence.md) · [Retention](storage-policies.md).

# MCP setup

Connect your client to `http://127.0.0.1:4170/mcp`.

```json
{"mcpServers":{"preflight":{"type":"http","url":"http://127.0.0.1:4170/mcp"}}}
```

The [Codex plugin](codex-plugin.md) supplies this connection. Configure it once.
If API tokens are enabled, configure Bearer authentication in the client; never put tokens in Git.

## Register, save, run

Replace `PROJECT_ID` and `RUN_ID` with returned IDs. Paths must be inside allowed roots.

`projects_register`

<!-- mcp: projects_register -->
```json
{
  "name": "my-project",
  "path": "/absolute/path/to/project"
}
```

`pipeline_save`

<!-- mcp: pipeline_save -->
```json
{
  "project_id": "PROJECT_ID",
  "name": "checks",
  "expected_revision": 0,
  "definition": {
    "version": 1,
    "pipeline": {
      "stages": [
        {
          "id": "test",
          "kind": "test",
          "command": {
            "program": "go",
            "args": [
              "test",
              "./..."
            ]
          }
        }
      ]
    }
  }
}
```

`pipeline_run_saved`

<!-- mcp: pipeline_run_saved -->
```json
{
  "project_id": "PROJECT_ID",
  "name": "checks",
  "revision": 1,
  "idempotency_key": "checks-1"
}
```

## Read the result

`build_report`

<!-- mcp: build_report -->
```json
{
  "run_id": "RUN_ID"
}
```

`logs_read`

<!-- mcp: logs_read -->
```json
{
  "run_id": "RUN_ID",
  "stage": "test",
  "after": 0
}
```

Reuse returned cursors and IDs. Read bounded logs for the failed stage; use
[completion events](completion-events.md) instead of waiting on a long build.

Saved revisions live in ignored `.preflight/`. Update with the returned revision
as `expected_revision`; stale writes fail. [Queues](change-queues.md) run several changes.
